//! Fleet manager (eve/client/script/parklife/fleetSvc.py).
//! Operates on process-local fleet state, including watchlist subscriptions.
//! Fleet advertising is handled separately by the fleet proxy.

use super::*;

fn ids(value: &Value) -> Option<Vec<i64>> {
    let mut result = Vec::new();
    for id in value.items()?.iter().map(Value::as_int) {
        let id = id?;
        if !result.contains(&id) { result.push(id); }
    }
    Some(result)
}

pub(super) fn update_watchlist(st: &mut State, me: i64, method: &str, args: &[Value]) -> Result<Option<Vec<i64>>, CallError> {
    let invalid = || notify_error(messages::fleets::INVALID_REQUEST);
    let fleet_id = *st.of.get(&me).ok_or_else(invalid)?;
    if !st.is_member(fleet_id, me) { return Err(invalid()); }
    let disable = method == "RegisterForDamageUpdates" && matches!(args.first(), Some(Value::None));
    let requested = if disable { Vec::new() } else if method == "RemoveFromWatchlist" {
        vec![args.first().and_then(Value::as_int).ok_or_else(invalid)?]
    } else {
        ids(args.first().ok_or_else(invalid)?).ok_or_else(invalid)?
    };
    let full = if method == "RegisterForDamageUpdates" {
        None
    } else {
        match args.get(1) {
            None | Some(Value::None) => None,
            Some(value) => Some(ids(value).ok_or_else(invalid)?),
        }
    };
    if requested.iter().chain(full.iter().flatten()).any(|&id| id == me || !st.is_member(fleet_id, id)) { return Err(invalid()); }
    let member = st.fleets.get_mut(&fleet_id).and_then(|f| f.members.get_mut(&me)).ok_or_else(invalid)?;
    let mut next = member.watchlist.clone();
    let mut added = Vec::new();
    match method {
        "AddToWatchlist" => for id in requested { if !next.contains(&id) { next.push(id); added.push(id); } },
        "RemoveFromWatchlist" => next.retain(|id| !requested.contains(id)),
        "RegisterForDamageUpdates" if !disable => next = requested,
        "RegisterForDamageUpdates" => {},
        _ => return Err(invalid()),
    }
    if next.len() > MAX_DAMAGE_SENDERS || full.as_ref().is_some_and(|ids| ids.len() > MAX_DAMAGE_SENDERS) { return Err(invalid()); }
    if let Some(full) = &full {
        if full.len() != next.len() || full.iter().any(|id| !next.contains(id)) { return Err(invalid()); }
        next.clone_from(full);
    }
    let was_enabled = member.damage_updates;
    let enabled = if method == "RegisterForDamageUpdates" { !disable } else { full.is_some() };
    member.watchlist = next;
    member.damage_updates = enabled;
    if enabled && (method == "RegisterForDamageUpdates" || !was_enabled) {
        return Ok(Some(member.watchlist.clone()));
    }
    Ok((enabled && !added.is_empty()).then_some(added))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "ForceLeaveFleet" => {
            lifecycle::leave(&c.world, c.session.int("charid").unwrap_or(0));
            Some(Ok(Value::None))
        }
        "AddToWatchlist" | "RemoveFromWatchlist" | "RegisterForDamageUpdates" => {
            let me = c.session.int("charid").unwrap_or(0);
            let result = update_watchlist(&mut state(), me, call.method, &call.args);
            Some(match result {
                Ok(initial) => {
                    if let Some(targets) = initial { super::damage::initial(c.world.clone(), me, targets).await; }
                    Ok(Value::None)
                }
                Err(error) => Err(error),
            })
        }
        "BroadcastToSystem" => Some(super::broadcasts::send(c, None, call, super::broadcasts::BroadcastDestination::System).await),
        "BroadcastToBubble" => Some(super::broadcasts::send(c, None, call, super::broadcasts::BroadcastDestination::Bubble).await),
        _ => None,
    }
}
