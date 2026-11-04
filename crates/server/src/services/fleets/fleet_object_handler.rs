//! Fleet object handler for `eve/client/script/parklife/fleetSvc.py`.
//! Fleet state is process-local; ship types are read from character and item data.
//! Saved fleet setups and boosts are not supported.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let world = c.world.clone();
    match (call.service, call.method) {
        ("fleetObjectHandler", "CreateFleet") if !call.bound => {
            let mut st = state();
            if me <= 0 || c.session.int("fleetid").is_some() || !st.can_create(me) {
                return Some(Err(CallError::User { msg: messages::MSG_FLEET_ERROR.into(), dict: vec![] }));
            }
            if let Some(old) = st.of.get(&me).copied() {
                lifecycle::remove_member_from(&mut st, &world, old, me);
            }
            st.clear_pending(me);
            let id = next_id();
            let (wing, squad) = (next_id(), next_id());
            let mut fleet = Fleet::new(me, vec![Wing { id: wing, name: messages::fleets::FLEET_INITIAL_WING_NAME.into(), squads: vec![Squad { id: squad, name: messages::fleets::FLEET_INITIAL_SQUAD_NAME.into() }] }]);
            fleet.creator_session = Some(c.session.sid);
            st.fleets.insert(id, fleet);
            drop(st);
            return Some(Ok(c.bind("fleetObjectHandler", Value::Int(id))));
        }
        _ => {}
    }
    let fleet_id = bound_fleet(call);
    if call.service != "fleetObjectHandler" { return None; }
    if !call.bound {
        return None;
    }
    if call.method == "Reconnect" {
        if me <= 0 || fleet_id <= 0 || !call.args.is_empty() || !(matches!(call.kwargs, Value::None) || matches!(call.kwargs, Value::Dict(v) if v.is_empty())) {
            return Some(Err(notify_error(messages::fleets::INVALID_REQUEST)));
        }
        let leader = {
            let st = state();
            st.fleets.get(&fleet_id).and_then(|f| f.members.iter().find_map(|(&id, m)| (m.role == ROLE_LEADER).then_some(id)))
        };
        let name = if let Some(id) = leader { crate::dao::characters::routing::character_name(&c.db, id).await.ok().flatten().unwrap_or_default() } else { String::new() };
        let mut st = state();
        if !st.has_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
        let f = st.fleets.get_mut(&fleet_id).unwrap();
        let leader = f.members.iter().find_map(|(&id, m)| (m.role == ROLE_LEADER).then_some(id)).unwrap_or(0);
        let member = f.members.get_mut(&me).unwrap();
        if member.reconnect.is_none() {
            return Some(if member.active_session == Some(c.session.sid) && member.recovered_generation.is_some() { Ok(Value::None) } else { Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET)) });
        }
        let reservation = member.reconnect.as_mut().unwrap();
        if Instant::now() >= reservation.deadline { return Some(Err(notify_error(messages::fleets::FLEET_INVITATION_EXPIRED))); }
        if reservation.offered_session == Some(c.session.sid) { return Some(Ok(Value::None)); }
        if reservation.offered_session.is_some() { return Some(Err(notify_error(messages::fleets::FLEET_INVITATION_EXPIRED))); }
        reservation.offered_session = Some(c.session.sid);
        c.notify("OnFleetInvite", invite_args(fleet_id, leader, &name, true));
        return Some(Ok(Value::None));
    }
    if call.method != "Init" {
        let st = state();
        let permitted = if matches!(call.method, "AcceptInvite" | "RejectInvite") {
            st.fleets.get(&fleet_id).is_some_and(|f| f.invites.contains_key(&me) || f.members.get(&me).is_some_and(|m| m.reconnect.as_ref().is_some_and(|r| r.offered_session == Some(c.session.sid)) || (call.method == "AcceptInvite" && m.active_session == Some(c.session.sid) && m.recovered_generation.is_some())))
        } else {
            st.is_member(fleet_id, me)
        };
        if !permitted {
            return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET)));
        }
    }
    Some(match call.method {
        // The creator occupies the fleet command slot, outside all wings and squads.
        "Init" => {
            let ship_type = match crate::dao::characters::routing::fleet_member_locations(&c.db, &[me]).await {
                Ok(rows) => rows.first().and_then(|r| r.type_id).map(i64::from).unwrap_or(0),
                Err(e) => return Some(Err(CallError::Internal(e.into()))),
            };
            let member = {
                let mut st = state();
                let Some(mut member) = st.initialize(fleet_id, me, ship_type) else {
                    return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET)));
                };
                member.active_session = Some(c.session.sid);
                st.fleets.get_mut(&fleet_id).unwrap().members.insert(me, member.clone());
                member
            };
            let pairs = session_pairs(Some(fleet_id), Some(&member));
            set_own_session(c, &pairs);
            info!("fleets: {me} formed fleet {fleet_id}");
            Ok(Value::None)
        }
        "GetFleetID" => Ok(Value::Int(fleet_id)),
        "GetInitState" => {
            let st = state();
            let Some(f) = st.fleets.get(&fleet_id) else { return Some(Err(notify_error(messages::fleets::FLEET_NOT_FOUND))) };
            Ok(keyval([
                ("fleetID", Value::Int(fleet_id)),
                ("members", Value::Dict(f.members.iter().map(|(ch, m)| (Value::Int(*ch), member_value(*ch, m))).collect())),
                ("wings", wings_value(f)),
                ("options", options_value(&f.options)),
                ("motd", Value::unicode(f.motd.as_str())),
            ]))
        }
        "GetWings" => match state().fleets.get(&fleet_id) {
            Some(f) => Ok(wings_value(f)),
            None => Ok(Value::Dict(vec![])),
        },
        "GetMotd" => Ok(Value::unicode(state().fleets.get(&fleet_id).map(|f| f.motd.clone()).unwrap_or_default())),
        "SetMotdEx" => {
            let motd = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    f.motd = motd.clone();
                    broadcast(&world, f, None, "OnFleetMotdChanged", vec![Value::unicode(motd), Value::Bool(true)]);
                    Ok(Value::None)
                }
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_FOR_MOTD)),
            }
        }
        "GetFleetComposition" => {
            let ids: Vec<i64> = state().fleets.get(&fleet_id).map(|f| f.members.keys().copied().collect()).unwrap_or_default();
            match crate::dao::characters::routing::fleet_member_locations(&c.db, &ids).await {
                Ok(rows) => Ok(Value::list(rows.iter().map(|r| {
                    keyval([
                        ("characterID", Value::Int(r.character_id)),
                        ("solarSystemID", r.solar_system_id.map(Value::Int).unwrap_or(Value::None)),
                        ("stationID", r.station_id.map(Value::Int).unwrap_or(Value::None)),
                        ("shipTypeID", r.type_id.map(|t| Value::Int(t as i64)).unwrap_or(Value::None)),
                        ("skills", Value::list([])),
                        ("skillIDs", Value::list([])),
                    ])
                }))),
                Err(e) => Err(CallError::Internal(e.into())),
            }
        }
        "Invite" => invite(c, fleet_id, int(0).unwrap_or(0), int(1), int(2), int(3)).await,
        // AcceptInvite(shipTypeID)
        "AcceptInvite" => {
            let recovery = {
                let st = state();
                let member = st.fleets.get(&fleet_id).and_then(|f| f.members.get(&me));
                if member.is_some_and(|m| m.reconnect.is_none() && m.active_session == Some(c.session.sid) && m.recovered_generation.is_some()) {
                    return Some(Ok(Value::None));
                }
                member.and_then(|m| m.reconnect.as_ref()).filter(|r| r.offered_session == Some(c.session.sid) && Instant::now() < r.deadline).map(|r| r.generation)
            };
            let ship_type = match crate::dao::characters::routing::fleet_member_locations(&c.db, &[me]).await {
                Ok(rows) => rows.first().and_then(|r| r.type_id).map(i64::from).unwrap_or(0),
                Err(e) => return Some(Err(CallError::Internal(e.into()))),
            };
            let joined = {
                let mut st = state();
                if let Some(generation) = recovery {
                    if !st.has_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::FLEET_INVITATION_EXPIRED))); }
                    let f = st.fleets.get_mut(&fleet_id).unwrap();
                    let m = f.members.get_mut(&me).unwrap();
                    if m.reconnect.is_none() && m.active_session == Some(c.session.sid) && m.recovered_generation == Some(generation) { return Some(Ok(Value::None)); }
                    if !m.reconnect.as_ref().is_some_and(|r| r.generation == generation && r.offered_session == Some(c.session.sid) && Instant::now() < r.deadline) {
                        return Some(Err(notify_error(messages::fleets::FLEET_INVITATION_EXPIRED)));
                    }
                    m.reconnect = None;
                    m.active_session = Some(c.session.sid);
                    m.recovered_generation = Some(generation);
                    m.ship_type = ship_type;
                    let m = m.clone();
                    broadcast(&world, f, Some(me), "OnFleetJoin", vec![member_value(me, &m)]);
                    st.clear_pending(me);
                    m
                } else {
                    if st.of.get(&me).is_some_and(|&old| old == fleet_id || st.is_member(old, me)) { return Some(Err(notify_error(messages::fleets::PILOT_ALREADY_IN_FLEET_SELF))); }
                    let Some(f) = st.fleets.get(&fleet_id) else { return Some(Err(notify_error(messages::fleets::FLEET_NOT_FOUND))) };
                    let Some(&(wing, squad, role)) = f.invites.get(&me) else { return Some(Err(notify_error(messages::fleets::FLEET_INVITATION_EXPIRED))) };
                    if f.members.len() >= f.max_members { return Some(Err(notify_error(messages::fleets::FLEET_FULL))); }
                    let mut m = Member::new(wing, squad, role, JOB_NONE, ship_type);
                    if !super::arrangement::valid_slot(f, me, &m) { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))); }
                    m.active_session = Some(c.session.sid);
                    if let Some(old) = st.of.get(&me).copied() { lifecycle::remove_member_from(&mut st, &world, old, me); }
                    st.clear_pending(me);
                    let f = st.fleets.get_mut(&fleet_id).unwrap();
                    f.members.insert(me, m.clone());
                    broadcast(&world, f, Some(me), "OnFleetJoin", vec![member_value(me, &m)]);
                    st.of.insert(me, fleet_id);
                    m
                }
            };
            let pairs = session_pairs(Some(fleet_id), Some(&joined));
            set_own_session(c, &pairs);
            if let Some(system) = c.session.int("solarsystemid") {
                world.post(system, move |actor| space_state::observe(actor, system, me));
            }
            info!("fleets: {me} joined fleet {fleet_id}");
            Ok(Value::None)
        }
        // RejectInvite(busy)
        "RejectInvite" => {
            let mut st = state();
            if st.has_member(fleet_id, me) && st.fleets[&fleet_id].members[&me].reconnect.as_ref().is_some_and(|r| r.offered_session == Some(c.session.sid) && Instant::now() < r.deadline) {
                lifecycle::remove_member_from(&mut st, &world, fleet_id, me);
                return Some(Ok(Value::None));
            }
            if let Some(f) = st.fleets.get_mut(&fleet_id) {
                if f.invites.remove(&me).is_some() {
                    broadcast(&world, f, None, "OnFleetJoinReject", vec![Value::Int(me), Value::None]);
                }
            }
            Ok(Value::None)
        }
        "SetTakesFleetWarp" | "SetAcceptsConduitJumpsValue" | "SetAcceptsRegroupValue" => {
            let value = match call.args.first() {
                Some(Value::Bool(value)) => *value,
                Some(Value::Int(0)) => false,
                Some(Value::Int(1)) => true,
                _ => return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))),
            };
            let mut st = state();
            if !st.is_member(fleet_id, me) {
                return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET)));
            }
            let f = st.fleets.get_mut(&fleet_id).unwrap();
            let member = f.members.get_mut(&me).unwrap();
            let before = member.clone();
            match call.method {
                "SetTakesFleetWarp" => member.opt_outs.accepts_fleet_warp = value,
                "SetAcceptsConduitJumpsValue" => member.opt_outs.accepts_conduit_jumps = value,
                _ => member.opt_outs.accepts_fleet_regroups = value,
            }
            let after = member.clone();
            broadcast(&world, f, None, "OnFleetMemberChanged", changed(me, fleet_id, &before, &after, f.members.len() == 1));
            Ok(Value::None)
        }
        "LeaveFleet" => {
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            lifecycle::remove_member_from(&mut st, &world, fleet_id, me);
            set_own_session(c, &session_pairs(None, None));
            Ok(Value::None)
        }
        "KickMember" => {
            let target = int(0).unwrap_or(0);
            let mut st = state();
            let allowed = st.is_member(fleet_id, me) && st.has_member(fleet_id, target) && st.fleets.get(&fleet_id).is_some_and(|f| is_leader(f, me));
            if !allowed {
                return Some(Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_REMOVE_MEMBER)));
            }
            lifecycle::remove_member_from(&mut st, &world, fleet_id, target);
            Ok(Value::None)
        }
        "DisbandFleet" => {
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            let members: Vec<i64> = match st.fleets.get(&fleet_id) {
                Some(f) if is_leader(f, me) => f.members.keys().copied().filter(|&id| id != me).chain(std::iter::once(me)).collect(),
                _ => return Some(Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_DISBAND))),
            };
            for ch in members {
                lifecycle::remove_member_from(&mut st, &world, fleet_id, ch);
            }
            Ok(Value::None)
        }
        // MakeLeader(charID) / MoveMember(charID, wingID, squadID, role)
        "MakeLeader" | "MoveMember" | "MassMoveMembers" => {
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            let mass = call.method == "MassMoveMembers";
            let ids: Vec<i64> = if mass {
                let Some(values) = call.args.first().and_then(Value::items) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                let Some(ids) = values.iter().map(Value::as_int).collect() else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                ids
            } else { vec![int(0).unwrap_or(0)] };
            if ids.iter().any(|id| !st.has_member(fleet_id, *id)) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET_TARGET))); }
            let f = st.fleets.get_mut(&fleet_id).unwrap();
            let transfer = !mass && (call.method == "MakeLeader" || int(3) == Some(ROLE_LEADER)) && ids[0] != me;
            if !is_leader(f, me) && !(f.options.free_move && !mass && ids == [me] && call.method == "MoveMember") {
                return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED)));
            }
            let mut changes = Vec::new();
            for id in &ids {
                let Some(before) = f.members.get(id) else { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET_TARGET))) };
                let mut after = before.clone();
                if call.method == "MakeLeader" {
                    after.wing = NONE_ID; after.squad = NONE_ID; after.role = ROLE_LEADER; after.job |= JOB_CREATOR;
                } else {
                    after.wing = int(1).unwrap_or(before.wing);
                    after.squad = int(2).unwrap_or(before.squad);
                    after.role = int(3).unwrap_or(before.role);
                    if transfer { after.job |= JOB_CREATOR; }
                    if !is_leader(f, me) && (before.role != ROLE_MEMBER || after.role != ROLE_MEMBER) {
                        return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED)));
                    }
                }
                changes.push((*id, after));
            }
            if transfer {
                let Some((wing, squad)) = f.wings.iter().find_map(|w| w.squads.first().map(|s| (w.id, s.id))) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                let mut old_leader = f.members[&me].clone();
                old_leader.wing = wing; old_leader.squad = squad; old_leader.role = ROLE_MEMBER; old_leader.job &= !JOB_CREATOR;
                changes.push((me, old_leader));
            }
            let Some(changes) = super::arrangement::apply(f, changes) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
            if transfer { super::fleet_proxy::leader_changed(&world, f, ids[0]); }
            let changes: Vec<_> = changes.into_iter().filter(|(_, before, after)| (before.wing, before.squad, before.role, before.job) != (after.wing, after.squad, after.role, after.job)).collect();
            let moved: Vec<_> = changes.iter().map(|(id, _, _)| Value::Int(*id)).collect();
            super::arrangement::publish(&world, fleet_id, f, changes);
            if mass { Ok(Value::list(moved)) } else { Ok(Value::None) }
        }
        "CreateWing" => {
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    if f.wings.len() >= MAX_WINGS { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))); }
                    let id = next_id();
                    let n = f.wings.len() + 1;
                    f.wings.push(Wing { id, name: messages::fleets::fleet_wing_name(n), squads: vec![] });
                    broadcast(&world, f, None, "OnFleetWingAdded", vec![Value::Int(id)]);
                    Ok(Value::Int(id))
                }
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_ADD_WING)),
            }
        }
        // CreateSquad(wingID)
        "CreateSquad" => {
            let wing = int(0).unwrap_or(0);
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let id = next_id();
                    let Some(w) = f.wings.iter_mut().find(|w| w.id == wing) else { return Some(Err(notify_error(messages::fleets::FLEET_WING_NOT_FOUND))) };
                    if w.squads.len() >= MAX_SQUADS { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))); }
                    let n = w.squads.len() + 1;
                    w.squads.push(Squad { id, name: messages::fleets::fleet_squad_name(n) });
                    broadcast(&world, f, None, "OnFleetSquadAdded", vec![Value::Int(wing), Value::Int(id)]);
                    Ok(Value::Int(id))
                }
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_ADD_SQUAD)),
            }
        }
        "DeleteWing" | "DeleteSquad" => {
            let id = int(0).unwrap_or(0);
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let exists = if call.method == "DeleteWing" { f.wings.iter().any(|w| w.id == id) } else { f.wings.iter().any(|w| w.squads.iter().any(|s| s.id == id)) };
                    if !exists { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))); }
                    let occupied = f.members.values().any(|m| m.wing == id || m.squad == id);
                    if occupied {
                        return Some(Err(notify_error(messages::fleets::FLEET_STRUCTURE_MUST_BE_EMPTY)));
                    }
                    let old_options = options_value(&f.options);
                    if f.options.auto_join_squad.is_some_and(|s| s == id || f.wings.iter().any(|w| w.id == id && w.squads.iter().any(|sq| sq.id == s))) {
                        f.options.auto_join_squad = None;
                    }
                    if call.method == "DeleteWing" {
                        f.wings.retain(|w| w.id != id);
                        broadcast(&world, f, None, "OnFleetWingDeleted", vec![Value::Int(id)]);
                    } else {
                        for w in &mut f.wings {
                            w.squads.retain(|s| s.id != id);
                        }
                        broadcast(&world, f, None, "OnFleetSquadDeleted", vec![Value::Int(id)]);
                    }
                    if old_options != options_value(&f.options) { broadcast(&world, f, None, "OnFleetOptionsChanged", vec![old_options, options_value(&f.options)]); }
                    Ok(Value::None)
                }
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_CHANGE_STRUCTURE)),
            }
        }
        // ChangeWingName(wingID, name) / ChangeSquadName(squadID, name)
        "ChangeWingName" | "ChangeSquadName" => {
            let id = int(0).unwrap_or(0);
            let name = value_text(call.args.get(1), MAX_NAME_LENGTH);
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            match st.fleets.get_mut(&fleet_id) {
                Some(f) if is_leader(f, me) => {
                    let wing = call.method == "ChangeWingName";
                    if !f.rename_structure(id, &name, wing) {
                        return Some(Err(notify_error(if wing { messages::fleets::FLEET_WING_NOT_FOUND } else { messages::fleets::INVALID_REQUEST })));
                    }
                    let event = if wing { "OnFleetWingNameChanged" } else { "OnFleetSquadNameChanged" };
                    broadcast(&world, f, None, event, vec![Value::Int(id), Value::unicode(name)]);
                    Ok(Value::None)
                }
                _ => Err(notify_error(messages::fleets::FLEET_LEADER_REQUIRED_TO_RENAME_WING_OR_SQUAD)),
            }
        }
        "GetFleetMaxSize" | "GetJoinRequests" | "SetOptions" | "SetAutoJoinSquadID" | "SetFleetMaxSize" | "RejectJoinRequest" | "LoadFleetSetup" => {
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            let f = st.fleets.get_mut(&fleet_id).unwrap();
            if !is_leader(f, me) { return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED))); }
            match call.method {
                "GetFleetMaxSize" => Ok(Value::Int(f.max_members as i64)),
                "GetJoinRequests" => Ok(Value::Dict(f.join_requests.iter().map(|(id, v)| (Value::Int(*id), v.clone())).collect())),
                "LoadFleetSetup" => Err(notify_error(messages::fleets::LOAD_SETUP_UNSUPPORTED)),
                "RejectJoinRequest" => {
                    let target = int(0).unwrap_or(0);
                    if f.join_requests.remove(&target).is_some() {
                        world.notify_pilot(target, "OnFleetJoinRejected", vec![Value::Int(target)]);
                        let requests = Value::Dict(f.join_requests.iter().map(|(id, v)| (Value::Int(*id), v.clone())).collect());
                        world.notify_pilot(me, "OnJoinRequestUpdate", vec![requests]);
                    }
                    Ok(Value::None)
                }
                "SetFleetMaxSize" => {
                    let Some(size) = int(0).and_then(|n| usize::try_from(n).ok()).filter(|n| *n >= f.members.len() && *n >= 1 && *n <= MAX_MEMBERS) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                    f.max_members = size;
                    Ok(Value::None)
                }
                "SetAutoJoinSquadID" => {
                    let squad = match call.args.first() {
                        Some(Value::None) => None,
                        Some(v) => match v.as_int() { Some(NONE_ID) => None, Some(id) => Some(id), None => return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) },
                        None => return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))),
                    };
                    if squad.is_some_and(|id| !f.wings.iter().any(|w| w.squads.iter().any(|s| s.id == id))) { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))); }
                    let old = options_value(&f.options);
                    f.options.auto_join_squad = squad;
                    broadcast(&world, f, None, "OnFleetOptionsChanged", vec![old, options_value(&f.options)]);
                    Ok(Value::None)
                }
                _ => {
                    let value = call.args.first().and_then(|v| crate::services::value_field(v, "isFreeMove")).and_then(bool_value);
                    let Some(value) = value else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                    let old = options_value(&f.options);
                    f.options.free_move = value;
                    broadcast(&world, f, None, "OnFleetOptionsChanged", vec![old, options_value(&f.options)]);
                    Ok(Value::None)
                }
            }
        }
        "FinishMove" => {
            let member = {
                let st = state();
                if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
                st.fleets[&fleet_id].members[&me].clone()
            };
            set_own_session(c, &session_pairs(Some(fleet_id), Some(&member)));
            Ok(Value::None)
        }
        "UpdateMemberInfo" => {
            let ship_type = match crate::dao::characters::routing::fleet_member_locations(&c.db, &[me]).await {
                Ok(rows) => rows.first().and_then(|r| r.type_id).map(i64::from).unwrap_or(0),
                Err(e) => return Some(Err(CallError::Internal(e.into()))),
            };
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            let f = st.fleets.get_mut(&fleet_id).unwrap();
            let member = f.members.get_mut(&me).unwrap();
            let before = member.clone();
            member.ship_type = ship_type;
            let after = member.clone();
            broadcast(&world, f, None, "OnFleetMemberChanged", changed(me, fleet_id, &before, &after, f.members.len() == 1));
            Ok(Value::None)
        }
        "ArrangeMembersByShipType" | "ArrangeMembersByPreCount" | "ArrangeMembersByEvenlyDistribute" | "ArrangeMembersByAllToFirst" => {
            let mut st = state();
            if !st.is_member(fleet_id, me) { return Some(Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET))); }
            let f = st.fleets.get_mut(&fleet_id).unwrap();
            if !is_leader(f, me) { return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED))); }
            let wing = int(0).unwrap_or(NONE_ID);
            if !f.wings.iter().any(|w| w.id == wing && !w.squads.is_empty()) { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))); }
            let mut ids: Vec<_> = f.members.iter().filter(|(_, m)| m.wing == wing && m.role == ROLE_MEMBER).map(|(id, _)| *id).collect();
            ids.sort_unstable();
            if call.method == "ArrangeMembersByShipType" {
                let Some(types) = call.args.get(1).and_then(Value::items) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                let Some(types): Option<Vec<i64>> = types.iter().map(Value::as_int).collect() else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                if types.is_empty() { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))); }
                ids.retain(|id| types.contains(&f.members[id].ship_type));
                if call.args.get(3).and_then(bool_value).unwrap_or(false) { return Some(Ok(Value::Int(ids.len() as i64))); }
                if call.args.get(2).and_then(bool_value).unwrap_or(false) { return Some(Err(notify_error(messages::fleets::ARRANGE_REMOVAL_UNSUPPORTED))); }
            }
            let per = if call.method == "ArrangeMembersByPreCount" {
                let Some(n) = int(1).and_then(|v| usize::try_from(v).ok()).filter(|n| *n > 0) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
                n
            } else { 1 };
            let Some(changes) = super::arrangement::plan(f, wing, &ids, call.method, per) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
            let Some(changes) = super::arrangement::apply(f, changes) else { return Some(Err(notify_error(messages::fleets::INVALID_REQUEST))) };
            super::arrangement::publish(&world, fleet_id, f, changes);
            Ok(Value::Bool(true))
        }
        "SendBroadcast" => super::broadcasts::send(c, Some(fleet_id), call, super::broadcasts::BroadcastDestination::Universe).await,
        _ => return None,
    })
}

fn invite_args(fleet_id: i64, inviter: i64, name: &str, auto_accept: bool) -> Vec<Value> {
    let question = messages::fleets::fleet_invitation(name);
    vec![Value::Int(fleet_id), Value::Int(inviter), Value::str(messages::MSG_CUSTOM_QUESTION), Value::dict([("header", Value::unicode(messages::fleets::FLEET_INVITATION_HEADER)), ("question", Value::unicode(question)), ("autoAccept", Value::Bool(auto_accept))])]
}

pub(super) fn notify_invite(world: &World, fleet_id: i64, inviter: i64, target: i64, name: &str) {
    world.notify_pilot(target, "OnFleetInvite", invite_args(fleet_id, inviter, name, false));
}

pub(super) async fn invite(c: &mut Client, fleet_id: i64, target: i64, wing: Option<i64>, squad: Option<i64>, role: Option<i64>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let name = crate::dao::characters::routing::character_name(&c.db, me).await.ok().flatten().unwrap_or_default();
    {
        let mut st = state();
        if !valid_invite_target(me, target, c.world.is_online(target), c.world.online_session(target).is_some()) {
            return Err(notify_error(messages::fleets::INVALID_REQUEST));
        }
        if !st.is_member(fleet_id, me) { return Err(notify_error(messages::fleets::PILOT_NOT_IN_FLEET)); }
        if st.of.get(&target).is_some_and(|&old| old == fleet_id || st.is_member(old, target)) { return Err(notify_error(messages::fleets::PILOT_ALREADY_IN_FLEET)); }
        let f = st.fleets.get_mut(&fleet_id).unwrap();
        if !is_leader(f, me) { return Err(notify_error(messages::fleets::LEADER_REQUIRED)); }
        if f.members.len() >= f.max_members { return Err(notify_error(messages::fleets::FLEET_FULL)); }
        let default = f.options.auto_join_squad.and_then(|id| f.wings.iter().find(|w| w.squads.iter().any(|s| s.id == id)).map(|w| (w.id, id)))
            .or_else(|| f.wings.iter().find_map(|w| w.squads.first().map(|s| (w.id, s.id))));
        let (default_wing, default_squad) = default.unwrap_or((NONE_ID, NONE_ID));
        let (wing, squad, role) = (wing.unwrap_or(default_wing), squad.unwrap_or(default_squad), role.unwrap_or(ROLE_MEMBER));
        let member = Member::new(wing, squad, role, JOB_NONE, 0);
        if role == ROLE_LEADER || !super::arrangement::valid_slot(f, target, &member) { return Err(notify_error(messages::fleets::INVALID_REQUEST)); }
        f.invites.insert(target, (wing, squad, role));
        if f.join_requests.remove(&target).is_some() {
            let requests = Value::Dict(f.join_requests.iter().map(|(id, value)| (Value::Int(*id), value.clone())).collect());
            c.world.notify_pilot(me, "OnJoinRequestUpdate", vec![requests]);
        }
    }
    notify_invite(&c.world, fleet_id, me, target, &name);
    Ok(Value::None)
}

pub(super) fn valid_invite_target(inviter: i64, target: i64, online: bool, has_session: bool) -> bool {
    target > 0 && target != inviter && online && has_session
}

fn bool_value(value: &Value) -> Option<bool> {
    match value { Value::Bool(v) => Some(*v), Value::Int(0) => Some(false), Value::Int(1) => Some(true), _ => None }
}
