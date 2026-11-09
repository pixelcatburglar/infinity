//! Agent manager service and dialogue.
//! Successful acceptance queues an event after the existing state and cargo writes.
//! Courier and trade cargo objectives use packaged item volume from the loaded SDE,
//! matching `agentinteraction/objectivesteps.py`; unsupported mission kinds have no cargo objective.
//! Epic arc journal messages mirror `evemissions.common.epic_arcs_data` and use
//! the loaded SDE journal rows.
use super::*;
// ---------------------------------------------------------------- client shapes

fn keywords(m: &Mission) -> Value {
    Value::dict([("dungeonLocationID", Value::Int(m.system_id)), ("dungeonSolarSystemID", Value::Int(m.system_id))])
}

/// A location for agentDialogueUtil.LocationWrapper.
fn site_location(m: &Mission) -> Value {
    Value::dict([
        ("locationID", Value::Int(m.system_id)),
        ("locationType", Value::str("dungeon")),
        ("solarsystemID", Value::Int(m.system_id)),
        ("typeID", Value::Int(TYPE_SOLAR_SYSTEM as i64)),
        ("coords", Value::tuple(m.pos.map(Value::Float))),
        ("agentID", Value::Int(m.agent_id)),
    ])
}

/// The agent's station: (stationID, typeID, solarSystemID, position).
async fn agent_station(c: &Client, agent: &Agent) -> Result<(i64, i64, i64, V3), CallError> {
    Ok(match c.sde.map.station(agent.station_id) {
        Some(r) => (agent.station_id, r.type_id.expect("station type_id is NULL"), r.solar_system_id.expect("station solar_system_id is NULL"), [r.x.unwrap_or(0.0), r.y.unwrap_or(0.0), r.z.unwrap_or(0.0)]),
        None => (agent.station_id, 0, agent.system_id, [0.0; 3]),
    })
}

/// Where an agent sits, as a LocationWrapper dict.
async fn agent_location(c: &Client, agent_id: i64) -> Result<Value, CallError> {
    let Some(a) = agent(agent_id) else { return Ok(Value::None) };
    let (station, ty, system, _) = agent_station(c, a).await?;
    Ok(Value::dict([
        ("locationID", Value::Int(station)),
        ("locationType", Value::str("station")),
        ("solarsystemID", Value::Int(system)),
        ("typeID", Value::Int(ty)),
    ]))
}

/// GetMissionObjectiveInfo: kill missions list their site, the others their hand-in
/// (agentDialogueUtil._ProcessObjectiveEntry: 'agent', 'transport', 'fetch').
async fn objective_info(c: &Client, m: &Mission) -> Result<Value, CallError> {
    if m.kind == Kind::Kill {
        return Ok(kill_objective_info(c, m).await);
    }
    let hand_in = m.hand_in_agent();
    let mut objectives = vec![];
    let cargo_value = |has: bool, (type_id, qty): (i32, i32), volume: f64| {
        Value::dict([("hasCargo", Value::Bool(has)), ("quantity", Value::Int(qty as i64)), ("typeID", Value::Int(type_id as i64)), ("volume", Value::Float(volume))])
    };
    let volume = |c: &Client, (type_id, qty): (i32, i32)| crate::services::item_volume::item_volume(&c.sde, type_id, false, i64::from(qty), None);
    match (m.kind, m.cargo) {
        (Kind::Talk, _) => objectives.push(Value::tuple([Value::str("agent"), Value::tuple([Value::Int(hand_in), agent_location(c, hand_in).await?])])),
        (Kind::Courier, Some(cargo)) => {
            let station = agent(hand_in).map(|a| a.station_id).unwrap_or(0);
            let has = has_goods(c, station, cargo).await? || m.state == STATE_ACCEPTED;
            let data = Value::tuple([
                Value::Int(m.agent_id),
                agent_location(c, m.agent_id).await?,
                Value::Int(hand_in),
                agent_location(c, hand_in).await?,
                cargo_value(has, cargo, volume(c, cargo)?),
            ]);
            objectives.push(Value::tuple([Value::str("transport"), data]));
        }
        (Kind::Trade, Some(cargo)) => {
            let station = agent(hand_in).map(|a| a.station_id).unwrap_or(0);
            let has = has_goods(c, station, cargo).await?;
            let data = Value::tuple([Value::Int(hand_in), agent_location(c, hand_in).await?, cargo_value(has, cargo, volume(c, cargo)?)]);
            objectives.push(Value::tuple([Value::str("fetch"), data]));
        }
        _ => {}
    }
    let system = agent(hand_in).map(|a| a.system_id).unwrap_or(m.system_id);
    Ok(Value::dict([
        ("importantStandings", Value::Int(0)),
        ("completionStatus", Value::Int(0)),
        ("missionTitleID", Value::Int(m.name_id)),
        ("missionState", Value::Int(m.state as i64)),
        ("objectives", Value::List(objectives)),
        ("dungeons", Value::list([])),
        ("locations", Value::list([Value::Int(system)])),
        ("agentGift", Value::list([])),
        ("normalRewards", Value::list([Value::tuple([Value::Int(TYPE_CREDITS), Value::Int(m.reward_isk), Value::None])])),
        ("loyaltyPoints", Value::Int(m.reward_lp)),
        ("researchPoints", Value::Int(0)),
        ("bonusRewards", Value::list([])),
        ("collateral", Value::list([])),
        ("contentID", Value::Int(m.mission_id as i64)),
    ]))
}

async fn kill_objective_info(c: &Client, m: &Mission) -> Value {
    let done = objective_done(c, m).await;
    let mut dungeon = vec![
        ("dungeonID", Value::Int(m.dungeon_id as i64)),
        ("optional", Value::Bool(false)),
        ("objectiveCompleted", if done { Value::Int(1) } else { Value::None }),
        ("location", site_location(m)),
        ("ownerID", Value::Int(m.faction_id)),
    ];
    if done {
        dungeon.push(("completionStatus", Value::Int(1)));
    }
    Value::dict([
        ("importantStandings", Value::Int(0)),
        ("completionStatus", Value::Int(i64::from(done))),
        ("missionTitleID", Value::Int(m.name_id)),
        ("missionState", Value::Int(m.state as i64)),
        ("objectives", Value::list([])),
        ("dungeons", Value::list([Value::dict(dungeon)])),
        ("locations", Value::list([Value::Int(m.system_id)])),
        ("agentGift", Value::list([])),
        ("normalRewards", Value::list([Value::tuple([Value::Int(TYPE_CREDITS), Value::Int(m.reward_isk), Value::None])])),
        ("loyaltyPoints", Value::Int(m.reward_lp)),
        ("researchPoints", Value::Int(0)),
        ("bonusRewards", Value::list([])),
        ("collateral", Value::list([])),
        ("contentID", Value::Int(m.mission_id as i64)),
    ])
}

/// appConst.agentMissionBriefing* keys.
fn briefing_info(m: &Mission) -> Value {
    Value::dict([
        ("ContentID", Value::Int(m.mission_id as i64)),
        ("Decline Time", Value::None),
        ("Expiration Time", Value::None),
        ("Mission Title ID", Value::Int(m.name_id)),
        ("Mission Briefing ID", Value::Int(m.briefing_id)),
        ("Mission Keywords", keywords(m)),
        ("Mission Image", Value::unicode("")),
        ("AcceptTimestamp", m.accepted_at.map(Value::Int).unwrap_or(Value::None)),
    ])
}

/// A journal row: (missionState, important, missionTypeLabel, missionNameID, agentID,
/// expirationTime, bookmarks, remoteOfferable, remoteCompletable, contentID).
async fn journal_row(c: &Client, m: &Mission) -> Result<Value, CallError> {
    let mut bookmarks = vec![];
    if let Some(agent) = agent(m.agent_id) {
        let (station, station_type, station_system, pos) = agent_station(c, agent).await?;
        bookmarks.push(keyval([
            ("agentID", Value::Int(m.agent_id)),
            ("locationType", Value::str("agenthomebase")),
            ("locationNumber", Value::Int(0)),
            ("hint", Value::str("UI/Agents/Bookmarks/AgentBase")),
            ("locationID", Value::Int(station_system)),
            ("itemID", Value::Int(station)),
            ("typeID", Value::Int(station_type)),
            ("x", Value::Float(pos[0])),
            ("y", Value::Float(pos[1])),
            ("z", Value::Float(pos[2])),
            ("solarsystemID", Value::Int(station_system)),
        ]));
    }
    if m.state == STATE_ACCEPTED && m.kind == Kind::Kill {
        bookmarks.push(keyval([
            ("agentID", Value::Int(m.agent_id)),
            ("locationType", Value::str("dungeon")),
            ("locationNumber", Value::Int(0)),
            ("hint", Value::str("UI/Agents/Bookmarks/Encounter/Deadspace")),
            ("locationID", Value::Int(m.system_id)),
            ("itemID", Value::Int(m.system_id)),
            ("typeID", Value::Int(TYPE_SOLAR_SYSTEM as i64)),
            ("x", Value::Float(m.pos[0])),
            ("y", Value::Float(m.pos[1])),
            ("z", Value::Float(m.pos[2])),
            ("solarsystemID", Value::Int(m.system_id)),
        ]));
    }
    Ok(Value::tuple([
        Value::Int(m.state as i64),
        Value::Bool(false),
        Value::str(m.kind.journal_label(m.arc_id.is_some())),
        Value::Int(m.name_id),
        Value::Int(m.agent_id),
        Value::None,
        Value::List(bookmarks),
        Value::Bool(false),
        Value::Bool(false),
        Value::Int(m.mission_id as i64),
    ]))
}

async fn journal(c: &Client, agent_id: Option<i64>) -> CallResult {
    let mut rows = vec![];
    for m in missions_of(c, agent_id).await? {
        rows.push(journal_row(c, &m).await?);
    }
    // (missions, research)
    Ok(Value::tuple([Value::List(rows), Value::list([])]))
}

/// {messageType: {missionID: messageID}} for every epic arc journal message
/// (evemissions.common.epic_arcs_data MESSAGE_*). The epic arc controller asks once per
/// mission node; the projected data is shared and the wire value is built per call.
async fn epic_arc_messages(c: &Client) -> CallResult {
    let mut by_type: Vec<(String, Vec<(Value, Value)>)> = vec![];
    for r in c.sde.missions.journal() {
        let kind = r.key.clone();
        let entry = (Value::Int(i64::from(r.mission_id)), Value::Int(r.message_id.expect("epic arc journal message_id is NULL")));
        match by_type.last_mut() {
            Some((k, list)) if *k == kind => list.push(entry),
            _ => by_type.push((kind, vec![entry])),
        }
    }
    Ok(Value::Dict(by_type.into_iter().map(|(k, list)| (Value::str(k), Value::Dict(list))).collect()))
}

/// A mission message `(messageID, contentID)`, or `fallback` text when the mission has none.
async fn says(c: &Client, m: &Mission, key: &str, fallback: &str) -> Result<Value, CallError> {
    Ok(match message(c, m.mission_id, key).await? {
        Some(id) => Value::tuple([Value::Int(id), Value::Int(m.mission_id as i64)]),
        None => Value::unicode(fallback),
    })
}

fn actions(list: &[(i64, i64)]) -> Value {
    Value::list(list.iter().map(|(id, button)| Value::tuple([Value::Int(*id), Value::Int(*button)])))
}

fn last_action(completed: bool, declined: bool, quit: bool) -> Value {
    Value::dict([
        ("missionCompleted", Value::Bool(completed)),
        ("missionDeclined", Value::Bool(declined)),
        ("missionQuit", Value::Bool(quit)),
        ("missionCantReplay", Value::None),
        ("loyaltyPoints", Value::Int(0)),
    ])
}

/// An agent interaction's answers as special-interaction actions: each shows its
/// message as a link (agentDialogueWindow.GetSpecialInteractionHTML).
async fn option_actions(c: &Client, m: &Mission) -> Result<Vec<Value>, CallError> {
    Ok(options(c, m.mission_id)
        .await?
        .into_iter()
        .enumerate()
        .map(|(i, msg)| {
            let briefing = Value::dict([
                ("ContentID", Value::Int(m.mission_id as i64)),
                ("Mission Title ID", Value::Int(msg)),
                ("Mission Briefing ID", Value::None),
                ("Mission Keywords", keywords(m)),
            ]);
            Value::tuple([Value::Int(ACT_OPTION + i as i64), briefing])
        })
        .collect())
}

/// What the agent says about a mission it has with us, and what we can do about it.
async fn view(c: &Client, m: &Mission, agent: &Agent) -> Result<(Value, Value), CallError> {
    Ok(if m.state == STATE_OFFERED && m.kind == Kind::Interaction {
        let mut acts = option_actions(c, m).await?;
        acts.push(Value::tuple([Value::Int(ACT_DECLINE), Value::Int(BTN_DECLINE)]));
        (Value::tuple([Value::Int(m.briefing_id), Value::Int(m.mission_id as i64)]), Value::List(acts))
    } else if m.state == STATE_OFFERED {
        (
            Value::tuple([Value::Int(m.briefing_id), Value::Int(m.mission_id as i64)]),
            actions(&[(ACT_ACCEPT, BTN_ACCEPT), (ACT_DECLINE, BTN_DECLINE)]),
        )
    } else if ready(c, m, agent).await? {
        let done = if m.kind == Kind::Kill { messages::MISSION_TARGET_CLEARED_COMPLETE } else { messages::MISSION_READY_COMPLETE };
        (Value::unicode(done), actions(&[(ACT_COMPLETE, BTN_COMPLETE), (ACT_QUIT, BTN_QUIT)]))
    } else if m.kind != Kind::Kill {
        let hint = match m.kind {
            Kind::Talk | Kind::Courier if m.dest_agent.is_some() => messages::MISSION_FIND_AGENT_DELIVER,
            Kind::Courier | Kind::Trade => messages::MISSION_DELIVER_TO_STATION_HANGAR,
            _ => messages::MISSION_INCOMPLETE,
        };
        (says(c, m, "messages.root.missioninprogress.agentsays", hint).await?, actions(&[(ACT_QUIT, BTN_QUIT)]))
    } else {
        (
            says(c, m, "messages.root.missioninprogress.agentsays", messages::MISSION_TARGET_REMAINS).await?,
            actions(&[(ACT_QUIT, BTN_QUIT)]),
        )
    })
}

fn conversation(says: Value, acts: Value, last: Value) -> CallResult {
    // ((agentSays, actions), lastActionInfo)
    Ok(Value::tuple([Value::tuple([says, acts]), last]))
}

async fn do_action(c: &mut Client, agent: &Agent, action: Option<i64>) -> CallResult {
    let none = || last_action(false, false, false);
    // Talk-to-agent and courier missions another agent gave us, handed in here.
    let mut delivery = None;
    for m in missions_of(c, None).await? {
        if m.agent_id != agent.id && ready(c, &m, agent).await? {
            delivery = Some(m);
            break;
        }
    }
    if let Some(m) = delivery {
        if action == Some(ACT_DELIVER) {
            finish(c, &m, agent, None).await?;
            let s = says(c, &m, "messages.mission.completed.agentsays", messages::MISSION_WELL_DONE).await?;
            return conversation(s, actions(&[(ACT_REQUEST, BTN_REQUEST)]), last_action(true, false, false));
        }
        if matches!(action, None | Some(ACT_REQUEST)) {
            return conversation(Value::unicode(messages::MISSION_WAITING_FOR_PILOT), actions(&[(ACT_DELIVER, BTN_COMPLETE)]), none());
        }
    }
    let arc_agent = is_arc_agent(c, agent.id).await?;
    if agent.type_id != AGENT_TYPE_BASIC && !arc_agent {
        return conversation(Value::unicode(messages::NO_MISSION_AVAILABLE), Value::list([]), none());
    }
    let current = mission_with(c, agent.id).await?;
    match (action, current) {
        (None, None) => conversation(Value::unicode(messages::MISSION_GREETING_WITH_WORK), actions(&[(ACT_REQUEST, BTN_REQUEST)]), none()),
        (None, Some(_)) => conversation(Value::unicode(messages::MISSION_INCOMPLETE), actions(&[(ACT_VIEW, BTN_VIEW)]), none()),
        (Some(ACT_REQUEST), None) if arc_agent => {
            let Some(step) = next_arc_step(c, agent.id).await? else {
                return conversation(Value::unicode(messages::NO_CURRENT_MISSION), Value::list([]), none());
            };
            offer_arc(c, agent, step).await?;
            c.notify("OnAgentMissionChange", vec![Value::str("offered"), Value::Int(agent.id)]);
            let m = mission_with(c, agent.id).await?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("offer vanished")))?;
            let (s, a) = view(c, &m, agent).await?;
            conversation(s, a, none())
        }
        (Some(ACT_REQUEST), None) => {
            let me = c.session.int("charid").unwrap_or(0);
            let needed = super::standings::required_for_level(agent.level);
            let have = super::standings::effective(&c.db, me, agent.id, agent.corp_id, agent.faction_id).await?;
            if have < needed {
                let text = messages::mission_standing_requirement_message(needed, have);
                return conversation(Value::unicode(text), Value::list([]), none());
            }
            offer(c, agent).await?;
            c.notify("OnAgentMissionChange", vec![Value::str("offered"), Value::Int(agent.id)]);
            let m = mission_with(c, agent.id).await?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("offer vanished")))?;
            let (s, a) = view(c, &m, agent).await?;
            conversation(s, a, none())
        }
        (Some(ACT_REQUEST | ACT_VIEW), Some(m)) => {
            let (s, a) = view(c, &m, agent).await?;
            conversation(s, a, none())
        }
        (Some(choice), Some(m)) if choice >= ACT_OPTION && m.state == STATE_OFFERED && m.kind == Kind::Interaction => {
            finish(c, &m, agent, Some((choice - ACT_OPTION) as usize)).await?;
            let s = says(c, &m, "messages.mission.completed.agentsays", messages::MISSION_ACCEPTED_CONFIRMATION).await?;
            conversation(s, actions(&[(ACT_REQUEST, BTN_REQUEST)]), last_action(true, false, false))
        }
        (Some(ACT_ACCEPT), Some(m)) if m.state == STATE_OFFERED => {
            let me = c.session.int("charid").unwrap_or(0);
            let (agent_id, station, mission_id, arc) = (agent.id, agent.station_id, m.mission_id, m.arc_id);
            let cargo = if m.kind == Kind::Courier { m.cargo } else { None };
            let stamp = filetime_now();
            let _lock = super::super::market::lock().await;
            let db = c.db.clone();
            let sde = c.sde.clone();
            db.transaction(
                move |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let sde = sde.clone();
                    Box::pin(async move {
                        if crate::dao::missions::accept_offered(tx, me, agent_id, mission_id, STATE_OFFERED, STATE_ACCEPTED, stamp).await.map_err(db_err)? == 0 {
                            return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                        }
                        if let Some(arc) = arc {
                            crate::dao::missions::set_arc_state(tx, me, arc, mission_id, "accepted", Some(stamp)).await.map_err(db_err)?;
                        }
                        if let Some((type_id, qty)) = cargo {
                            super::items::deposit(tx, &sde, outbox, &crate::services::inventory::destination::Destination::owned(me, station, FLAG_HANGAR), type_id, i64::from(qty), 0).await?;
                        }
                        outbox.event(crate::lua::events::GameEvent::MissionAccepted { char_id: me, agent_id, mission_id });
                        outbox.notify_client("OnAgentMissionChange", vec![Value::str("accepted"), Value::Int(agent_id)]);
                        Ok(())
                    })
                },
                |value, outbox| {
                    let client = &mut *c;
                    async move { crate::db::outbox::dispatch_client(value, outbox, client) }
                },
            ).await?;
            drop(_lock);
            info!(user = c.login.user_id, "agent {}: mission {} accepted", agent.id, m.mission_id);
            let s = says(c, &m, "messages.mission.accepted.agentsays", messages::MISSION_ACCEPTED_DESTINATION_LOGGED).await?;
            conversation(s, actions(&[(ACT_QUIT, BTN_QUIT)]), none())
        }
        (Some(ACT_DECLINE), Some(m)) if m.state == STATE_OFFERED => {
            remove(c, agent.id).await?;
            let me = c.session.int("charid").unwrap_or(0);
            let mut outbox = crate::db::outbox::Outbox::default();
            super::standings::mission_declined(&c.db, &mut outbox, me, agent.id, agent.corp_id).await?;
            crate::db::outbox::dispatch_client((), outbox, c)?;
            c.notify("OnAgentMissionChange", vec![Value::str("offer_declined"), Value::Int(agent.id)]);
            let s = says(c, &m, "messages.mission.declined.agentsays", messages::MISSION_DECLINED).await?;
            conversation(s, Value::list([]), last_action(false, true, false))
        }
        (Some(ACT_COMPLETE), Some(m)) if ready(c, &m, agent).await? => {
            finish(c, &m, agent, None).await?;
            let s = says(c, &m, "messages.mission.completed.agentsays", messages::MISSION_REWARD_DELIVERY).await?;
            let next = if m.arc_id.is_some() { actions(&[(ACT_REQUEST, BTN_REQUEST)]) } else { Value::list([]) };
            conversation(s, next, last_action(true, false, false))
        }
        (Some(ACT_QUIT), Some(m)) => {
            remove(c, agent.id).await?;
            let me = c.session.int("charid").unwrap_or(0);
            let mut outbox = crate::db::outbox::Outbox::default();
            super::standings::mission_declined(&c.db, &mut outbox, me, agent.id, agent.corp_id).await?;
            crate::db::outbox::dispatch_client((), outbox, c)?;
            // A quit arc step can be taken up again (its fail mission, usually itself).
            if let Some(arc) = m.arc_id {
                set_arc_state(c, arc, m.mission_id, "quit").await?;
                let retry = c.sde.missions.step(m.mission_id).and_then(|s| s.fail).unwrap_or(m.mission_id);
                set_arc_state(c, arc, retry, "unlocked").await?;
            }
            c.notify("OnAgentMissionChange", vec![Value::str("quit"), Value::Int(agent.id)]);
            let s = says(c, &m, "messages.mission.quitted.agentsays", messages::MISSION_QUIT).await?;
            conversation(s, Value::list([]), last_action(false, false, true))
        }
        (_, current) => match current {
            Some(m) => {
                let (s, a) = view(c, &m, agent).await?;
                conversation(s, a, none())
            }
            None => conversation(Value::unicode(messages::PILOT_GREETING), actions(&[(ACT_REQUEST, BTN_REQUEST)]), none()),
        },
    }
}

/// WarpToLocation(locationType, locationNumber, warpRange, fleet, referringAgentID, formation)
async fn warp_to_location(c: &mut Client, agent: &Agent, call: &Call<'_>) -> CallResult {
    let kind = call.args.first().and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
    let range = call.args.get(2).and_then(|v| v.as_float()).unwrap_or(0.0);
    let order = if call.args.get(3).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0)) {
        Some(super::super::fleets::warp::authorize(c).await?)
    } else { None };
    let here = c.session.int("solarsystemid");
    match kind.as_str() {
        "dungeon" => {
            let Some(m) = mission_with(c, agent.id).await?.filter(|m| m.state == STATE_ACCEPTED && m.kind == Kind::Kill) else { return Ok(Value::None) };
            if here != Some(m.system_id) {
                return Ok(Value::None);
            }
            ensure_site(c, &m).await?;
            super::ship::warp_destination(&crate::services::identity::Identity::of_client(c), &c.db, &c.world, here, order, crate::space::system::warp::WarpDestination::Ordinary { point: m.pos, object_radius: None, min_range: range }, None, Value::Dict(vec![])).await
        }
        "agenthomebase" => {
            let (station, _, system, pos) = agent_station(c, agent).await?;
            if here != Some(system) {
                return Ok(Value::None);
            }
            let radius = c.world.with(system, move |s| s.ball(station).map(|b| b.radius as f64)).await.flatten().unwrap_or(FALLBACK_STATION_RADIUS);
            super::ship::warp_destination(&crate::services::identity::Identity::of_client(c), &c.db, &c.world, here, order, crate::space::system::warp::WarpDestination::Ordinary { point: pos, object_radius: Some(radius), min_range: range }, None, Value::dict([("celestialID", Value::Int(station))])).await
        }
        _ => Ok(Value::None),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.method == "GetMessagesForEpicArcMissions" {
        return Some(epic_arc_messages(c).await);
    }
    // The bound agent: Moniker('agentMgr', agentID).
    let bound_agent = if call.bound { call.bind_params.and_then(|p| p.as_int()).and_then(agent) } else { None };
    let Some(agent) = bound_agent else {
        return match call.method {
            "GetMyJournalDetails" => Some(journal(c, None).await),
            "GetMyEpicArcStatus" => Some(epic_arc_status(c).await),
            "GetSolarSystemOfAgent" => {
                let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
                Some(Ok(super::agent(id).map(|a| Value::Int(a.system_id)).unwrap_or(Value::None)))
            }
            // Mission IDs switched off server-side (agents.IsMissionDisabled): none.
            "GetDisabledMissions" => Some(Ok(Value::list([]))),
            // Show info on an NPC that isn't an agent (a corporation CEO, say): no agent
            // details, which the info window skips.
            "GetInfoServiceDetails" => Some(Ok(Value::None)),
            _ => None,
        };
    };
    let me = c.session.int("charid").unwrap_or(0);
    let current = match mission_with(c, agent.id).await {
        Ok(m) => m,
        Err(e) => return Some(Err(e)),
    };
    Some(match call.method {
        "DoAction" => {
            let action = call.args.first().and_then(|v| v.as_int());
            do_action(c, agent, action).await
        }
        "GetMissionBriefingInfo" => Ok(current.as_ref().map(briefing_info).unwrap_or(Value::None)),
        "GetMissionObjectiveInfo" => match &current {
            Some(m) => objective_info(c, m).await,
            None => Ok(Value::None),
        },
        "GetMissionJournalInfo" => match &current {
            Some(m) => match objective_info(c, m).await {
                Ok(objectives) => Ok(Value::dict([
                    ("missionNameID", Value::Int(m.name_id)),
                    ("contentID", Value::Int(m.mission_id as i64)),
                    ("briefingTextID", Value::Int(m.briefing_id)),
                    ("missionImage", Value::unicode("")),
                    ("expirationTime", Value::None),
                    ("missionState", Value::Int(m.state as i64)),
                    ("objectives", objectives),
                ])),
                Err(e) => Err(e),
            },
            None => Ok(Value::None),
        },
        "GetMissionKeywords" => Ok(current.as_ref().map(keywords).unwrap_or(Value::Dict(vec![]))),
        "GetAgentLocationWrap" => agent_station(c, agent).await.map(|(station, ty, system, _)| {
            Value::dict([
                ("locationID", Value::Int(station)),
                ("locationType", Value::str("station")),
                ("solarsystemID", Value::Int(system)),
                ("typeID", Value::Int(ty)),
            ])
        }),
        "GetMyJournalDetails" => journal(c, Some(agent.id)).await,
        "WarpToLocation" => warp_to_location(c, agent, call).await,
        "GotoLocation" | "GetEntryPoint" | "GetReplayTimestamp" | "GetDungeonShipRestrictions" => Ok(Value::None),
        "IsCheatingWithAgent" | "ShouldAlwaysAllowReplay" => Ok(Value::Bool(false)),
        "RemoveOfferFromJournal" => {
            if current.as_ref().is_some_and(|m| m.state == STATE_OFFERED) {
                if let Err(e) = remove(c, agent.id).await {
                    return Some(Err(e));
                }
                c.notify("OnAgentMissionChange", vec![Value::str("offer_removed"), Value::Int(agent.id)]);
            }
            Ok(Value::None)
        }
        "GetInfoServiceDetails" => Ok(keyval([
            ("stationID", Value::Int(agent.station_id)),
            ("level", Value::Int(agent.level)),
            ("services", Value::list([])),
            ("incompatible", Value::None),
        ])),
        _ => {
            let _ = me;
            return None;
        }
    })
}

#[cfg(test)]
mod tests;
