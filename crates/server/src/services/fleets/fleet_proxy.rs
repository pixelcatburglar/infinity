//! Runtime fleet finder for `fleetSvc.py` and `evefleet/fleetAdvertObject.py`.
//! Adverts use live sessions and character contacts/security; no advert survives restart.
//! The remote eligibility algorithm is unavailable: the best leader-owned contact
//! relationship for an applicant's character or affiliations is the substitute.

use super::*;
use crate::dao::characters::{contacts, crimewatch, routing};
use infinity_core::session::OnlineSession;
use crate::services::{db_err, value_field};
use std::collections::{HashSet, VecDeque};

// evefleet.const invitation scope bits.
const CORP: i64 = 1;
const ALLIANCE: i64 = 2;
const MILITIA: i64 = 4;
const PUBLIC: i64 = 8;
const PUBLIC_OPEN: i64 = 16;
const SCOPE_MASK: i64 = CORP | ALLIANCE | MILITIA | PUBLIC | PUBLIC_OPEN;
// evefleet.fleetAdvertObject limits.
const MAX_ENTITIES: usize = 1000;
const MAX_TITLE: usize = 32;
const MAX_DESCRIPTION: usize = 250;

fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> { value_field(v, name) }
fn boolean(v: Option<&Value>, default: bool) -> Option<bool> {
    match v { None | Some(Value::None) => Some(default), Some(Value::Bool(b)) => Some(*b), Some(Value::Int(0)) => Some(false), Some(Value::Int(1)) => Some(true), _ => None }
}
fn number(v: Option<&Value>) -> Option<Option<f64>> {
    match v { None | Some(Value::None) => Some(None), Some(v) => v.as_float().filter(|n| n.is_finite()).map(Some) }
}
fn opt_id(v: Option<i64>) -> Value { v.map(Value::Int).unwrap_or(Value::None) }
fn leader_value(s: &OnlineSession) -> Value {
    keyval([("charID", Value::Int(s.character_id)), ("corpID", opt_id(s.corporation_id)), ("allianceID", opt_id(s.alliance_id)), ("warFactionID", opt_id(s.faction_id))])
}
fn set(v: &mut Value, name: &str, value: Value) {
    if let Value::Dict(entries) = v {
        if let Some((_, old)) = entries.iter_mut().find(|(k, _)| k.as_text().as_deref() == Some(name)) { *old = value; }
        else { entries.push((Value::str(name), value)); }
    }
}
fn text(v: Option<&Value>, max: usize) -> Option<Value> {
    match v { None | Some(Value::None) => Some(Value::unicode("")), Some(v) => Some(Value::unicode(v.as_text()?.chars().take(max).collect::<String>())) }
}
fn valid_entities(input: &Value, names: &[&str]) -> bool {
    names.iter().all(|name| match field(input, name) {
        None | Some(Value::None) => true,
        Some(v) => v.items().is_some_and(|values| values.len() <= MAX_ENTITIES && values.iter().all(|v| v.as_int().is_some_and(|id| id > 0))),
    })
}
pub(super) fn normalize(input: &Value, created: i64) -> Option<Value> {
    if !matches!(input, Value::Dict(_) | Value::Instance(_, _)) { return None; }
    // Client-supplied entity lists are validated but never used as authority.
    if !valid_entities(input, &["public_allowedEntities", "public_disallowedEntities", "membergroups_allowedEntities", "membergroups_disallowedEntities"]) { return None; }
    let scope = match field(input, "inviteScope") { None | Some(Value::None) => 0, Some(v) => v.as_int()? };
    if scope < 0 || scope & !SCOPE_MASK != 0 { return None; }
    let activity = match field(input, "activityValue") { None | Some(Value::None) => Value::None, Some(v) => Value::Int(v.as_int()?) };
    let limit = match field(input, "advertJoinLimit") { None | Some(Value::None) => Value::None, Some(v) => Value::Int(v.as_int()?.clamp(1, MAX_MEMBERS as i64)) };
    let mut result = Value::dict([
        ("fleetName", text(field(input, "fleetName"), MAX_TITLE)?),
        ("description", text(field(input, "description"), MAX_DESCRIPTION)?),
        ("inviteScope", Value::Int(scope)), ("activityValue", activity),
        ("advertJoinLimit", limit), ("dateCreated", Value::Int(created)),
        ("advertTime", Value::Int(crate::client::filetime_now())),
    ]);
    for name in ["useAdvanceOptions", "newPlayerFriendly", "joinNeedsApproval", "hideInfo", "updateOnBossChange"] {
        set(&mut result, name, Value::Bool(boolean(field(input, name), name == "updateOnBossChange")?));
    }
    for name in ["public_minStanding", "public_minSecurity", "membergroups_minStanding", "membergroups_minSecurity"] {
        set(&mut result, name, number(field(input, name))?.map(Value::Float).unwrap_or(Value::None));
    }
    for name in ["public_allowedEntities", "public_disallowedEntities", "membergroups_allowedEntities", "membergroups_disallowedEntities"] { set(&mut result, name, Value::list([])); }
    Some(result)
}
fn current_leader(f: &Fleet) -> Option<i64> { f.members.iter().find_map(|(id, m)| (m.role == ROLE_LEADER && m.reconnect.is_none()).then_some(*id)) }
pub(super) fn capacity(f: &Fleet, ad: &Value) -> bool {
    let limit = field(ad, "advertJoinLimit").and_then(Value::as_int).map(|n| n as usize).unwrap_or(MAX_MEMBERS);
    f.members.len() < f.max_members.min(limit)
}
fn matches_group(scope: i64, leader: &OnlineSession, applicant: &OnlineSession) -> bool {
    (scope & CORP != 0 && leader.corporation_id.is_some() && leader.corporation_id == applicant.corporation_id)
        || (scope & ALLIANCE != 0 && leader.alliance_id.is_some() && leader.alliance_id == applicant.alliance_id)
        || (scope & MILITIA != 0 && leader.faction_id.is_some() && leader.faction_id == applicant.faction_id)
}
fn identifiers(s: &OnlineSession) -> impl Iterator<Item = i64> {
    [Some(s.character_id), s.corporation_id, s.alliance_id, s.faction_id].into_iter().flatten()
}
pub(super) fn eligible(ad: &Value, leader: &OnlineSession, applicant: &OnlineSession, contacts: &[contacts::Contact], blocks: &[i64], reverse_blocks: &[i64], security: Option<f64>) -> Option<f64> {
    if identifiers(applicant).any(|id| blocks.contains(&id)) || reverse_blocks.contains(&leader.character_id) { return None; }
    let scope = field(ad, "inviteScope").and_then(Value::as_int).unwrap_or(0);
    let group = matches_group(scope, leader, applicant);
    if !group && scope & (PUBLIC | PUBLIC_OPEN) == 0 { return None; }
    let standing = contacts.iter().filter(|c| identifiers(applicant).any(|id| id == c.contact_id)).map(|c| c.relationship as f64).reduce(f64::max).unwrap_or(0.0);
    let prefix = if group { "membergroups" } else { "public" };
    let min_standing = field(ad, &format!("{prefix}_minStanding")).and_then(Value::as_float);
    let min_security = field(ad, &format!("{prefix}_minSecurity")).and_then(Value::as_float);
    if min_standing.is_some_and(|min| standing < min) || min_security.is_some_and(|min| security.is_none_or(|s| s < min)) { return None; }
    if !group && scope & PUBLIC == 0 && scope & PUBLIC_OPEN != 0 && standing <= 0.0 { return None; }
    Some(standing)
}
fn distance(map: &crate::sde::map::Map, from: Option<i64>, to: Option<i64>) -> Option<i64> {
    let (from, to) = (from?, to?);
    let mut seen = HashSet::from([from]);
    let mut queue = VecDeque::from([(from, 0)]);
    while let Some((id, d)) = queue.pop_front() {
        if id == to { return Some(d); }
        for &next in map.jumps(id) { if seen.insert(next) { queue.push_back((next, d + 1)); } }
    }
    None
}
pub(super) fn display(mut ad: Value, fleet_id: i64, count: usize, leader: &OnlineSession, standing: Option<f64>, jumps: Option<i64>, contacts: &[contacts::Contact], blocks: &[i64]) -> Value {
    set(&mut ad, "fleetID", Value::Int(fleet_id)); set(&mut ad, "leader", leader_value(leader));
    set(&mut ad, "solarSystemID", opt_id(leader.system_id)); set(&mut ad, "numMembers", Value::Int(count as i64));
    set(&mut ad, "standing", standing.map(Value::Float).unwrap_or(Value::None)); set(&mut ad, "numJumps", opt_id(jumps));
    for prefix in ["public", "membergroups"] {
        let min = field(&ad, &format!("{prefix}_minStanding")).and_then(Value::as_float);
        let scope = field(&ad, "inviteScope").and_then(Value::as_int).unwrap_or(0);
        let groups = [(CORP, leader.corporation_id), (ALLIANCE, leader.alliance_id), (MILITIA, leader.faction_id)];
        let mut allowed: Vec<i64> = if prefix == "membergroups" {
            groups.into_iter().filter_map(|(bit, id)| (scope & bit != 0).then_some(id).flatten()).collect()
        } else { Vec::new() };
        allowed.extend(contacts.iter().filter(|c| min.is_some_and(|m| c.relationship as f64 >= m)).map(|c| c.contact_id));
        allowed.sort_unstable(); allowed.dedup(); allowed.truncate(MAX_ENTITIES);
        let mut denied: Vec<i64> = blocks.to_vec();
        denied.extend(contacts.iter().filter(|c| min.is_some_and(|m| m <= 0.0 && (c.relationship as f64) < m)).map(|c| c.contact_id));
        denied.sort_unstable(); denied.dedup(); denied.truncate(MAX_ENTITIES);
        // fleetFinder_allowedEntities._FindDiff subtracts these values as Python sets.
        set(&mut ad, &format!("{prefix}_allowedEntities"), Value::reduce("__builtin__.set", vec![Value::list(allowed.into_iter().map(Value::Int))], None));
        set(&mut ad, &format!("{prefix}_disallowedEntities"), Value::reduce("__builtin__.set", vec![Value::list(denied.into_iter().map(Value::Int))], None));
    }
    ad
}

pub(super) fn leader_changed(world: &World, fleet: &mut Fleet, new_leader: i64) {
    let Some(ad) = fleet.advert.as_mut() else { return };
    if !boolean(field(ad, "updateOnBossChange"), false).unwrap_or(false) {
        let old = options_value(&fleet.options);
        fleet.advert = None; fleet.options.registered = false; fleet.join_requests.clear();
        broadcast(world, fleet, None, "OnFleetOptionsChanged", vec![old, options_value(&fleet.options)]);
    } else {
        let session = world.online_session(new_leader);
        set(ad, "leader", session.as_ref().map(leader_value).unwrap_or(Value::None));
        set(ad, "solarSystemID", session.and_then(|s| s.system_id).map(Value::Int).unwrap_or(Value::None));
    }
}

async fn facts(c: &Client, leader: i64, applicant: i64) -> Result<(Vec<contacts::Contact>, Vec<i64>, Vec<i64>, Option<f64>), CallError> {
    Ok((contacts::list(&c.db, leader).await.map_err(db_err)?, contacts::blocked(&c.db, leader).await.map_err(db_err)?, contacts::blocked(&c.db, applicant).await.map_err(db_err)?, crimewatch::security(&c.db, applicant).await.map_err(db_err)?))
}
fn invalid() -> CallError { notify_error(messages::fleets::INVALID_REQUEST) }
fn unavailable() -> CallError { notify_error(messages::fleets::AD_NOT_AVAILABLE) }
fn snapshot(fleet_id: i64) -> Option<(i64, usize, Value)> {
    let st = state(); let f = st.fleets.get(&fleet_id)?; let ad = f.advert.clone()?;
    Some((current_leader(f)?, f.members.len(), ad))
}
pub(super) fn still_current(f: &Fleet, leader: i64, count: usize, ad: &Value) -> bool {
    current_leader(f) == Some(leader) && f.members.len() == count && f.advert.as_ref() == Some(ad) && capacity(f, ad)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "fleetProxy" { return None; }
    let me = c.session.int("charid").unwrap_or(0);
    let applicant = match OnlineSession::from_session(&c.session) { Some(s) => s, None => return Some(Err(invalid())) };
    let own = || { let st = state(); st.of.get(&me).copied().filter(|id| st.is_member(*id, me)) };
    Some(match call.method {
        "GetAvailableFleetAds" => {
            let ids: Vec<i64> = state().fleets.keys().copied().collect();
            let mut ads = Vec::new();
            for id in ids {
                let Some((leader, count, ad)) = snapshot(id) else { continue };
                let Some(session) = c.world.online_session(leader) else { continue };
                let (contacts, blocks, reverse, security) = match facts(c, leader, me).await { Ok(v) => v, Err(e) => return Some(Err(e)) };
                let Some(standing) = eligible(&ad, &session, &applicant, &contacts, &blocks, &reverse, security) else { continue };
                if c.world.online_session(leader).as_ref() != Some(&session) || !state().fleets.get(&id).is_some_and(|f| still_current(f, leader, count, &ad)) { continue; }
                let jumps = distance(&c.world.sde().map, applicant.system_id, session.system_id);
                ads.push((Value::Int(id), display(ad, id, count, &session, Some(standing), jumps, &contacts, &blocks)));
            }
            Ok(Value::Dict(ads))
        }
        "GetMyFleetFinderAdvert" => {
            let Some(id) = own() else { return Some(Ok(Value::None)) };
            let Some((leader, count, ad)) = snapshot(id) else { return Some(Ok(Value::None)) };
            let Some(session) = c.world.online_session(leader) else { return Some(Ok(Value::None)) };
            let (contacts, blocks, _, _) = match facts(c, leader, me).await { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if c.world.online_session(leader).as_ref() != Some(&session) || !state().fleets.get(&id).is_some_and(|f| current_leader(f) == Some(leader) && f.members.len() == count && f.advert.as_ref() == Some(&ad)) { return Some(Err(unavailable())); }
            Ok(display(ad, id, count, &session, None, distance(&c.world.sde().map, applicant.system_id, session.system_id), &contacts, &blocks))
        }
        "AddFleetFinderAdvert" | "UpdateAdvertInfo" | "UpdateAdvertAllowedEntities" | "UpdateFleetAdvertWithNewLeader" => {
            let Some(id) = own() else { return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED))) };
            let (previous, count) = {
                let st = state();
                if !st.is_member(id, me) { return Some(Err(invalid())); }
                let f = st.fleets.get(&id).unwrap();
                if !is_leader(f, me) { return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED))); }
                (f.advert.clone(), f.members.len())
            };
            let ad = if call.method == "AddFleetFinderAdvert" {
                let Some(input) = call.args.first() else { return Some(Err(invalid())) };
                let created = previous.as_ref().and_then(|a| field(a, "dateCreated")).and_then(Value::as_int).unwrap_or_else(crate::client::filetime_now);
                let Some(ad) = normalize(input, created) else { return Some(Err(invalid())) };
                ad
            } else {
                let Some(ad) = previous.clone() else { return Some(Err(unavailable())) };
                let index = usize::from(call.method == "UpdateAdvertInfo");
                let Some(input) = call.args.get(index) else { return Some(Err(invalid())) };
                if !matches!(input, Value::Dict(_) | Value::Instance(_, _)) || !valid_entities(input, &[
                    "public_allowedEntities", "public_disallowedEntities", "membergroups_allowedEntities", "membergroups_disallowedEntities",
                    "publicAllowedEntities", "publicDisallowedEntities", "membergroupsAllowedEntities", "membergroupsDisallowedEntities",
                    "publicToAddToAllowed", "publicToRemoveFromAllowed", "publicToAddToDisallowed", "publicToRemoveFromDisallowed",
                    "membergroupsToAddToAllowed", "membergroupsToRemoveFromAllowed", "membergroupsToAddToDisallowed", "membergroupsToRemoveFromDisallowed",
                ]) { return Some(Err(invalid())); }
                // Client entity differences and member counts are hints; live contacts
                // and membership are the sole authority for the returned advert.
                ad
            };
            let Some(session) = c.world.online_session(me) else { return Some(Err(unavailable())) };
            let (contacts, blocks, _, _) = match facts(c, me, me).await { Ok(v) => v, Err(e) => return Some(Err(e)) };
            {
                let mut st = state();
                if !st.is_member(id, me) || c.world.online_session(me).as_ref() != Some(&session) { return Some(Err(unavailable())); }
                let f = st.fleets.get_mut(&id).unwrap();
                if !is_leader(f, me) || f.members.len() != count || f.advert != previous { return Some(Err(unavailable())); }
                let old = options_value(&f.options);
                f.advert = Some(ad.clone()); f.options.registered = true;
                if old != options_value(&f.options) { broadcast(&c.world, f, None, "OnFleetOptionsChanged", vec![old, options_value(&f.options)]); }
            }
            Ok(display(ad, id, count, &session, None, session.system_id.map(|_| 0), &contacts, &blocks))
        }
        "RemoveFleetFinderAdvert" => {
            let Some(id) = own() else { return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED))) };
            let mut st = state(); if !st.is_member(id, me) { return Some(Err(invalid())); }
            let f = st.fleets.get_mut(&id).unwrap(); if !is_leader(f, me) { return Some(Err(notify_error(messages::fleets::LEADER_REQUIRED))); }
            let old = options_value(&f.options);
            f.advert = None; f.options.registered = false; f.join_requests.clear();
            if old != options_value(&f.options) { broadcast(&c.world, f, None, "OnFleetOptionsChanged", vec![old, options_value(&f.options)]); }
            Ok(Value::None)
        }
        "ApplyToJoinFleet" => {
            let Some(id) = call.args.first().and_then(Value::as_int) else { return Some(Err(invalid())) };
            let Some(auto) = boolean(call.args.get(1), false) else { return Some(Err(invalid())) };
            let Some((leader, count, ad)) = snapshot(id) else { return Some(Err(unavailable())) };
            let Some(session) = c.world.online_session(leader) else { return Some(Err(unavailable())) };
            let (contacts, blocks, reverse, security) = match facts(c, leader, me).await { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if eligible(&ad, &session, &applicant, &contacts, &blocks, &reverse, security).is_none() { return Some(Err(unavailable())); }
            let name = match routing::character_name(&c.db, leader).await { Ok(v) => v.unwrap_or_default(), Err(e) => return Some(Err(db_err(e))) };
            let approval = boolean(field(&ad, "joinNeedsApproval"), false).unwrap_or(false) || !auto;
            {
                let mut st = state();
                if st.of.get(&me).is_some_and(|&old| old == id || st.is_member(old, me)) || c.world.online_session(leader).as_ref() != Some(&session) || OnlineSession::from_session(&c.session).as_ref() != Some(&applicant) { return Some(Err(unavailable())); }
                let Some(f) = st.fleets.get_mut(&id) else { return Some(Err(unavailable())) };
                if !still_current(f, leader, count, &ad) { return Some(Err(unavailable())); }
                if approval {
                    let info = keyval([("charID", Value::Int(me)), ("corpID", opt_id(applicant.corporation_id))]);
                    f.join_requests.insert(me, info.clone());
                    let requests = Value::Dict(f.join_requests.iter().map(|(id, v)| (Value::Int(*id), v.clone())).collect());
                    c.world.notify_pilot(leader, "OnFleetJoinRequest", vec![info]);
                    c.world.notify_pilot(leader, "OnJoinRequestUpdate", vec![requests]);
                } else {
                    let slot = f.options.auto_join_squad.and_then(|s| f.wings.iter().find(|w| w.squads.iter().any(|x| x.id == s)).map(|w| (w.id, s)))
                        .or_else(|| f.wings.iter().find_map(|w| w.squads.first().map(|s| (w.id, s.id))));
                    let Some((wing, squad)) = slot else { return Some(Err(unavailable())) };
                    if !super::arrangement::valid_slot(f, me, &Member::new(wing, squad, ROLE_MEMBER, JOB_NONE, 0)) { return Some(Err(unavailable())); }
                    f.invites.insert(me, (wing, squad, ROLE_MEMBER));
                    if f.join_requests.remove(&me).is_some() {
                        let requests = Value::Dict(f.join_requests.iter().map(|(id, v)| (Value::Int(*id), v.clone())).collect());
                        c.world.notify_pilot(leader, "OnJoinRequestUpdate", vec![requests]);
                    }
                }
            }
            if !approval { super::fleet_object_handler::notify_invite(&c.world, id, leader, me, &name); }
            Ok(Value::Bool(approval))
        }
        _ => return None,
    })
}
