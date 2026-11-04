//! Persistent drone inventory identities back client space balls and item notices.
//! Restoration prepares positioned drones before the system actor starts; orders
//! and controller state are transient, and unsupported drone work is refused.

use std::sync::Arc;

use crate::consts::{dogma::ATTR_AGILITY, dogma::ATTR_MASS, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_SIGNATURE_RADIUS, CATEGORY_DRONE, OWNER_UNOWNED};
use crate::dao::items::drones;
use crate::db::Db;
use crate::sde::Sde;
use crate::services::{db_err, CallError, fit::{Fit, Key}};
use crate::space::{Ball, MODE_STOP, WarpState, drone::DroneSpec};
pub(crate) mod launch;
pub(crate) mod returning;

fn drone_ball(fit: &Fit, key: Key, id: i64, type_id: i32, owner: i64, affiliation: (i64, i64), pos: [f64; 3], health: [f64; 3]) -> Result<Ball, CallError> {
    let data = fit.sde().types.get(type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("drone type missing from SDE")))?;
    let now = crate::space::now_ms();
    Ok(Ball {
        id, type_id, owner_id: owner,
        corp_id: affiliation.0, alliance_id: affiliation.1, char_id: None, mode: MODE_STOP,
        radius: data.radius.unwrap_or(0.0) as f32,
        signature_radius: Some(fit.value(key, ATTR_SIGNATURE_RADIUS)),
        mass: fit.value(key, ATTR_MASS), max_vel: fit.value(key, ATTR_MAX_VELOCITY) as f32,
        agility: fit.value(key, ATTR_AGILITY) as f32, speed_fraction: 0.0,
        pos, vel: [0.0; 3], old_pos: pos, old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: true, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: now,
        interpolation_anchor_ms: now, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health, launcher: 0, slim: Vec::new(),
    })
}

/// Transfer a live owned drone to neutral ownership without removing its ball.
pub(crate) async fn abandon(c: &mut crate::client::Client, id: i64) -> Result<(), CallError> {
    use crate::dao::items::loading::ItemQuery;
    use crate::db::outbox::{dispatch_client, Outbox};
    use crate::services::{items::load_items, notify_error};
    use marshal::Value;

    let refuse = || notify_error(crate::messages::drones::NOT_AVAILABLE);
    let owner = c.session.int("charid").ok_or_else(refuse)?;
    let system = c.session.int("solarsystemid").ok_or_else(refuse)?;
    let valid = c.world.with(system, move |s| s.drone(id).is_some_and(|d|
        d.owner == owner && s.ball(id).is_some())).await.unwrap_or(false);
    if !valid { return Err(refuse()); }
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let world = c.world.clone();
    let (outbox, task) = db.transaction(
        |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                let item = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?
                    .into_iter().next().ok_or_else(refuse)?;
                if item.owner_id != owner || item.location_id != system
                    || item.category_id != i64::from(CATEGORY_DRONE)
                    || !drones::abandon(tx, id, owner, system).await.map_err(db_err)? {
                    return Err(refuse());
                }
                crate::services::inventory::notifications::notify_item_change(tx, &sde, outbox, id,
                    vec![(crate::consts::IX_OWNER_ID, Value::Int(owner))]).await?;
                Ok(())
            })
        },
        |(), outbox| async move {
            let task = tokio::spawn(async move {
                if world.with(system, move |s| {
                    if s.drone(id).is_some_and(|d| d.owner == owner) { s.drone_abandoned(id); }
                }).await.is_none() {
                    match world.system(system).await {
                        Ok(()) => {
                            if world.with(system, move |s| {
                                if s.drone(id).is_some_and(|d| d.owner == owner) { s.drone_abandoned(id); }
                            }).await.is_none() {
                                tracing::warn!(system, "system actor stopped during committed abandon restore");
                            }
                        }
                        Err(error) => tracing::warn!(system, %error, "failed to restore committed abandoned drone"),
                    }
                }
            });
            Ok((outbox, task))
        },
    ).await?;
    if let Err(error) = task.await { tracing::warn!(system, %error, "committed drone abandon completion failed"); }
    if let Err(error) = dispatch_client((), outbox, c) {
        tracing::warn!(system, ?error, "committed drone abandon notification failed");
    }
    Ok(())
}

/// Load positioned singleton identities without entering or constructing a world.
pub(crate) async fn restore_specs(db: &Db, sde: Arc<Sde>, system: i64) -> Result<Vec<(Ball, DroneSpec)>, CallError> {
    let types: Vec<_> = sde.types.iter().filter(|data| data.category_id == Some(CATEGORY_DRONE)).map(|data| data.type_id).collect();
    let rows = drones::positioned(db, system, &types).await.map_err(db_err)?;
    let mut restored = Vec::with_capacity(rows.len());
    for row in rows {
        let (fit, key) = if row.owner_id == OWNER_UNOWNED {
            (Fit::for_space_type(sde.clone(), row.type_id, Vec::new()), Key::Ship)
        } else {
            (Fit::drone(db, sde.clone(), row.owner_id, row.item_id).await?, Key::Drone(row.item_id))
        };
        let spec = super::worker::spec(&fit, key, row.type_id)?;
        let health = crate::services::combat::drone_health::initialize(db, sde.clone(), row.owner_id, row.item_id, row.type_id).await?;
        let corp = if row.owner_id == OWNER_UNOWNED { OWNER_UNOWNED } else {
            crate::dao::characters::crimewatch::corporation(db, row.owner_id).await.map_err(db_err)?
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("drone owner corporation missing")))?
        };
        let ball = drone_ball(&fit, key, row.item_id, row.type_id, row.owner_id, (corp, -1), row.pos, health)?;
        restored.push((ball, spec));
    }
    Ok(restored)
}

#[cfg(test)] mod tests;
