//! GM preparation of accepted mission objectives using normal site and inventory mechanics.
//! The mission tracker consumes the same objective data as missionTrackerMgr.
//! Mission records remain in the game database until the player hands them in;
//! Site state remains transient, as for ordinary cleared sites. Branching interaction
//! missions require a player choice and are not supported by this command.
//! Actor effects run after database commit: a stopped actor can leave deposited
//! goods committed while the command reports failure.
//! Mission cargo deposits hold the market lock through transaction dispatch.
use super::*;
use std::collections::BTreeMap;
use crate::dao::items::loading::ItemQuery;
use crate::db::outbox::{Effect, Outbox};

fn add_requirement(goods: &mut BTreeMap<(i64, i32), i64>, station: i64, type_id: i32, qty: i64) -> Result<(), CallError> {
    let total = goods.entry((station, type_id)).or_default();
    *total = total.checked_add(qty)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("mission cargo requirement overflow")))?;
    Ok(())
}

fn missing_amount(required: i64, available: i128) -> i64 {
    (i128::from(required) - available).clamp(0, i128::from(required.max(0))) as i64
}

/// Make accepted mission objectives ready for their normal agent hand-in.
pub(crate) async fn force_complete(c: &mut Client, agent: Option<i64>) -> Result<usize, CallError> {
    let selected = missions_of(c, agent).await?;
    if agent.is_some() && selected.iter().any(|m| m.state == STATE_ACCEPTED && m.kind == Kind::Interaction) {
        return Err(CallError::User {
            msg: messages::MSG_SLASH_ERROR.into(),
            dict: vec![("reason".into(), Value::unicode(messages::gm::GM_MISSION_INTERACTION_UNSUPPORTED))],
        });
    }
    let missions: Vec<_> = selected.into_iter()
        .filter(|m| m.state == STATE_ACCEPTED && m.kind != Kind::Interaction).collect();
    let me = c.session.int("charid").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    let docked = c.session.int("stationid");
    let mut goods = BTreeMap::new();
    let mut clears = Vec::new();
    for m in &missions {
        if matches!(m.kind, Kind::Courier | Kind::Trade) {
            if let Some((type_id, qty)) = m.cargo {
                let station = super::agent(m.hand_in_agent())
                    .ok_or_else(|| CallError::Internal(anyhow::anyhow!("mission hand-in agent not found")))?.station_id;
                add_requirement(&mut goods, station, type_id, i64::from(qty))?;
            }
        }
        if m.kind == Kind::Kill {
            clears.push(sites::preflight_clear(c, m).await?);
        }
    }
    let db = c.db.clone();
    let sde = c.sde.clone();
    let _market = crate::services::market::lock().await;
    db.transaction(
        move |tx, outbox: &mut Outbox<(i64, i64)>| {
            let sde = sde.clone();
            let goods = goods.clone();
            let clears = clears.clone();
            Box::pin(async move {
                for clear in clears { outbox.operation(clear); }
                for ((station, type_id), qty) in goods {
                    let cargo_ship = if docked == Some(station) { ship } else { 0 };
                    let rows = items::load_items(tx, &sde, ItemQuery::OwnerTypeTwoLocations(me, type_id, station, FLAG_HANGAR, cargo_ship, FLAG_CARGO)).await.map_err(db_err)?;
                    let available = rows.iter().try_fold(0_i128, |total, item| total.checked_add(i128::from(item.quantity)))
                        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("mission inventory quantity overflow")))?;
                    let missing = missing_amount(qty, available);
                    if missing > 0 {
                        let destination = crate::services::inventory::destination::Destination::owned(me, station, FLAG_HANGAR);
                        items::deposit(tx, &sde, outbox, &destination, type_id, missing, 0).await?;
                    }
                }
                Ok(())
            })
        },
        |(), outbox| {
            let client = &mut *c;
            async move {
                let world = client.world.clone();
                let mut actor_error = None;
                for entry in outbox.into_entries() {
                    match entry {
                        Effect::Operation((agent_id, system)) => {
                            if let Err(error) = sites::force_clear(client, agent_id, system).await {
                                if actor_error.is_none() { actor_error = Some(error); }
                            }
                        }
                        Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *client))?,
                    }
                }
                if let Some(error) = actor_error { return Err(error); }
                Ok(())
            }
        },
    ).await?;
    let mut updates = Vec::new();
    for m in &missions {
        let info = mission_tracker_mgr::tracker_current(c, m).await
            .map(mission_tracker_mgr::strings).unwrap_or(Value::None);
        updates.push(Value::dict([("agentID", Value::Int(m.agent_id)), ("info", info)]));
    }
    if !updates.is_empty() { c.notify("OnMissionsUpdated", vec![Value::list(updates)]); }
    Ok(missions.len())
}

#[cfg(test)]
mod tests;
