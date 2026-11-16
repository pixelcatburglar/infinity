//! Client timer and security state methods for `crimewatch`.
//! Mirrors `eve/client/script/ui/services/crimewatchSvc.py`.
//! Security and the safety level persist in `game.characters`; timers and
//! engagements are in memory. `SetSafetyLevel` only writes the persisted
//! enum: the client updates its own safety afterwards, so there is no push,
//! session change or enforcement here. Security transaction history is not
//! modelled; flags are memory-only.

use std::collections::HashSet;
use super::*;

/// `GetClientStates` -> (combat timers, engagements, (criminals, suspects) in the system, safety level).
async fn client_states(c: &mut Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let safety = crate::dao::characters::crimewatch::safety(&c.db, me).await.map_err(db_err)?
        .ok_or_else(|| crate::services::notify_error(messages::crimewatch::CHARACTER_UNAVAILABLE))?;
    let here: HashSet<i64> = match c.session.int("solarsystemid") {
        Some(system) => pilots_in(&c.world, system).await.into_iter().collect(),
        None => HashSet::new(),
    };
    let now = filetime_now();
    let (timers, engagements, criminals, suspects, idle, naughty) = {
        let st = state();
        let pilot = st.pilots.get(&me);
        let running = |until: i64, on: i64, off: i64| if until > now { timer(on, Some(until)) } else { timer(off, None) };
        let timers = Value::tuple([
            running(pilot.map(|p| p.weapons_until).unwrap_or(0), WEAPONS_TIMER, WEAPONS_IDLE),
            running(pilot.map(|p| p.pvp_until).unwrap_or(0), PVP_TIMER, PVP_IDLE),
            running(pilot.map(|p| p.npc_until).unwrap_or(0), NPC_TIMER, NPC_IDLE),
            match pilot {
                Some(p) => { let (_, state, until) = effective_flag(p, now); timer(state, until) },
                None => timer(CRIMINAL_IDLE, None),
            },
            running(pilot.map(|p| p.disapproval_until).unwrap_or(0), DISAPPROVAL_TIMER, DISAPPROVAL_IDLE),
        ]);
        let engagements = Value::Dict(st.engagements.iter().filter(|((a, _), until)| *a == me && **until > now).map(|((_, b), until)| (Value::Int(*b), Value::Int(*until))).collect());
        let criminals: Vec<Value> = st.pilots.iter().filter(|(ch, p)| p.criminal_until > now && here.contains(ch)).map(|(ch, _)| Value::Int(*ch)).collect();
        let suspects: Vec<Value> = st.pilots.iter().filter(|(ch, p)| p.suspect_until > now && p.criminal_until <= now && here.contains(ch)).map(|(ch, _)| Value::Int(*ch)).collect();
        // The client's session change clears criminal flags but keeps cached
        // disapproval entries, so an active-only roster could never evict a
        // stale one: every pilot this state ever flagged reports idle when
        // not active here, and the live local roster reports naughty.
        let mut idle: Vec<i64> = st.pilots.iter()
            .filter(|(ch, p)| p.had_disapproval && !(p.disapproval_until > now && here.contains(ch)))
            .map(|(ch, _)| *ch).collect();
        let mut naughty: Vec<i64> = st.pilots.iter()
            .filter(|(ch, p)| p.disapproval_until > now && here.contains(ch))
            .map(|(ch, _)| *ch).collect();
        idle.sort_unstable();
        naughty.sort_unstable();
        (timers, engagements, criminals, suspects, idle, naughty)
    };
    c.world.notify_pilot(me, "OnSystemDisapprovalFlagUpdates", vec![
        Value::list(idle.into_iter().map(Value::Int)),
        Value::list(naughty.into_iter().map(Value::Int)),
    ]);
    Ok(Value::tuple([timers, engagements, Value::tuple([Value::List(criminals), Value::List(suspects)]), Value::Int(safety)]))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(match call.method {
        "GetClientStates" => client_states(c).await,
        "SetSafetyLevel" => {
            // crimewatchSvc.SetSafetyLevel sends exactly one integer enum;
            // bools, floats and wrong arities are refused, not coerced.
            let level = match call.args {
                [Value::Int(0)] => Some(control::SafetyLevel::None),
                [Value::Int(1)] => Some(control::SafetyLevel::Partial),
                [Value::Int(2)] => Some(control::SafetyLevel::Full),
                _ => None,
            };
            match level {
                Some(level) => control::set_safety(&c.db, c.session.int("charid").unwrap_or(0), level)
                    .await.map(|_| Value::None),
                None => Err(crate::services::notify_error(messages::crimewatch::SAFETY_LEVEL_INVALID)),
            }
        }
        "GetMySecurityStatus" => security_status(&c.db, c.session.int("charid").unwrap_or(0)).await.map(Value::Float),
        "GetCharacterSecurityStatus" => security_status(&c.db, int(0).unwrap_or(0)).await.map(Value::Float),
        "GetSecurityStatusTransactions" => Ok(Value::list([])),
        _ => return None,
    })
}
