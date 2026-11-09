use std::{future::Future, task::Poll, time::Duration};
use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{services::{Call, CallError, combat, effects::{registry::{self, EffectAttribute}, EffectKind, JammingType}, fit::{Fit, Key}}};
use super::fixture::Fixture;

fn error(e: CallError) -> anyhow::Error {
    match e { CallError::Internal(e) => e, CallError::User { msg, .. } => anyhow!("{msg}") }
}

async fn catch_scenario(future: impl Future<Output = Result<()>>) -> std::thread::Result<Result<()>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
        Ok(Poll::Pending) => Poll::Pending,
        Err(panic) => Poll::Ready(Err(panic)),
    }).await
}

async fn lock(f: &mut Fixture, owner: i64, target: i64) -> Result<()> {
    let args = [Value::Int(target)];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "AddTarget", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dogma::dispatch(&mut f.client, &call).await.map_err(error)?;
    tokio::time::timeout(Duration::from_secs(30), async {
        while !combat::targeting_charges::targets_of(owner).contains(&target) {
            ensure!(f.world.after(f.system, 100).await, "system stopped while locking");
        }
        Ok::<_, anyhow::Error>(())
    }).await.map_err(|_| anyhow!("lock timed out"))??;
    Ok(())
}

async fn command(f: &mut Fixture, method: &str, args: &[Value]) -> Result<()> {
    let value = f.call("entity", method, args).await.map_err(error)?;
    ensure!(value == Value::Dict(vec![]), "{method}: {value:?}");
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn published_ewar_drones_apply_real_effects_and_retire() -> Result<()> {
    let mut f = Fixture::new().await;
    let primary = (f.owner, f.ship);
    let mut receiver = None;
    let result = catch_scenario(async {
        let (victim, ship) = f.add_pilot().await;
        receiver = Some((victim, ship));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(victim, tx);
        for (item, attr, value) in [
            (f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0),
            (f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0),
            (f.ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
            (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
            (f.ship, ATTR_MAX_LOCKED_TARGETS, 10.0),
            (f.ship, ATTR_SIGNATURE_RADIUS, 1000.0),
            (ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
            (ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
            (ship, ATTR_MAX_LOCKED_TARGETS, 10.0),
            (ship, ATTR_TRACKING_SPEED, 1.0),
            (ship, ATTR_MAX_RANGE, 10000.0),
            (ship, ATTR_MAX_VELOCITY, 1000.0),
            (ship, ATTR_SIGNATURE_RADIUS, 1000.0),
            (ship, ATTR_CAPACITOR_CAPACITY, 10000.0),
            (ship, ATTR_RECHARGE_RATE, 1_000_000_000.0),
            (ship, ATTR_SCAN_GRAVIMETRIC_STRENGTH, 10.0),
        ] { f.set(item, attr, value).await; }
        lock(&mut f, primary.0, ship).await?;
        f.client.session.apply(vec![("charid", Value::Int(victim)), ("shipid", Value::Int(ship))]);
        lock(&mut f, victim, primary.1).await?;
        f.client.session.apply(vec![("charid", Value::Int(primary.0)), ("shipid", Value::Int(primary.1))]);

        let families = [
            EffectKind::Projected { jamming: JammingType::Webify },
            EffectKind::Projected { jamming: JammingType::Paint },
            EffectKind::Projected { jamming: JammingType::Damp },
            EffectKind::Projected { jamming: JammingType::TrackingDisrupt },
            EffectKind::Neutralizer,
            EffectKind::Ecm,
        ];
        for kind in families {
            let (ty, effect) = f.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
                && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0))
                .find_map(|t| {
                    let first = t.effects().iter().filter_map(|e| registry::resolve(&f.sde, e.effect_id)).find(|e| matches!(e.kind,
                        EffectKind::Turret | EffectKind::Missile | EffectKind::Mining | EffectKind::RemoteRepair { .. } | EffectKind::Neutralizer | EffectKind::Ecm |
                        EffectKind::Projected { jamming: JammingType::Webify | JammingType::Paint | JammingType::Damp | JammingType::TrackingDisrupt }))?;
                    (first.kind == kind).then_some((t.type_id, first))
                }).ok_or_else(|| anyhow!("no published drone for {kind:?}"))?;
            f.train_drone(ty).await;
            let drone = f.drone(ty, 1, true).await;
            let owner_fit = Fit::drone(&f.db, f.sde.clone(), primary.0, drone).await.map_err(error)?;
            ensure!(owner_fit.value(Key::Char, ATTR_DRONE_CONTROL_DISTANCE) >= 1000.0, "insufficient fitted control range");
            f.set(drone, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
            f.set(drone, ATTR_MAX_VELOCITY, 100_000.0).await;
            f.set(drone, ATTR_MAX_RANGE, 100_000.0).await;
            f.set(drone, effect.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE), 2000.0).await;
            if let Some(attr) = effect.attribute(EffectAttribute::Range) { f.set(drone, attr, 100_000.0).await; }
            if kind == EffectKind::Neutralizer { f.set(drone, ATTR_ENERGY_NEUTRALIZER_AMOUNT, 5000.0).await; }
            if kind == EffectKind::Ecm {
                for name in ["scanGravimetricStrengthBonus", "scanLadarStrengthBonus", "scanMagnetometricStrengthBonus", "scanRadarStrengthBonus"] {
                    let attr = f.sde.dogma.attributes_by_name(name).next().ok_or_else(|| anyhow!("missing {name}"))?.attribute_id;
                    f.set(drone, attr, 1_000_000.0).await;
                }
            }
            let mut module = None;
            let attrs: Vec<i32> = match kind {
                EffectKind::Projected { jamming: JammingType::Webify } => vec![ATTR_MAX_VELOCITY],
                EffectKind::Projected { jamming: JammingType::Paint } => vec![ATTR_SIGNATURE_RADIUS],
                EffectKind::Projected { jamming: JammingType::Damp } => vec![ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION],
                EffectKind::Projected { jamming: JammingType::TrackingDisrupt } => vec![ATTR_TRACKING_SPEED, ATTR_MAX_RANGE],
                _ => vec![],
            };
            let modified: Vec<i32> = f.sde.dogma.effect(effect.id as i32).map(|row| row.modifiers().iter()
                .filter(|m| m.domain == "targetID" && attrs.contains(&m.modified)).map(|m| m.modified).collect()).unwrap_or_default();
            if !attrs.is_empty() { ensure!(!modified.is_empty(), "{kind:?} has no expected target modifier"); }
            if kind == (EffectKind::Projected { jamming: JammingType::TrackingDisrupt }) {
                let turret = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
                    && t.attribute(ATTR_TRACKING_SPEED).is_some_and(|v| v > 0.0)
                    && f.sde.dogma.effect(effect.id as i32).is_some_and(|row| row.modifiers().iter().any(|m| {
                        m.domain == "targetID" && attrs.contains(&m.modified) && match m.func.as_str() {
                            "LocationGroupModifier" => m.group == t.group_id,
                            "LocationRequiredSkillModifier" => m.skill.is_some_and(|skill| ATTR_REQUIRED_SKILLS.iter().any(|&a| t.attribute(a) == Some(skill as f64))),
                            "LocationModifier" | "ItemModifier" => true,
                            _ => false,
                        }
                    }))
                    && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Turret)))
                    .ok_or_else(|| anyhow!("no published turret"))?.type_id;
                let id: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&turret, &victim, &ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
                f.set(id, ATTR_TRACKING_SPEED, 1.0).await;
                f.set(id, ATTR_MAX_RANGE, 10000.0).await;
                module = Some(id);
            }
            let keys: Vec<Key> = std::iter::once(Key::Ship).chain(module.map(Key::Module)).collect();
            let before = Fit::load_in_space(&f.db, &f.world, f.system, victim, ship).await.map_err(error)?;
            let baseline: Vec<_> = keys.iter().flat_map(|&key| modified.iter().map(move |&attr| (key, attr))).map(|(key, attr)| (key, attr, before.value(key, attr))).collect();
            let cap_before = combat::groups_health::capacitor_snapshot(&f.db, &f.world, f.system, ship).await.map_err(error)?.ok_or_else(|| anyhow!("missing capacitor"))?.level;
            let launch = [Value::list([Value::tuple([Value::Int(drone), Value::Int(1)])]), Value::None, Value::Bool(false)];
            let value = f.call("ship", "LaunchDrones", &launch).await.map_err(error)?;
            ensure!(value == Value::Dict(vec![(Value::Int(drone), Value::list([Value::Int(drone)]))]), "launch {kind:?}: {value:?}");
            command(&mut f, "CmdEngage", &[Value::list([Value::Int(drone)]), Value::Int(ship)]).await?;
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let done = if kind == EffectKind::Neutralizer {
                        combat::groups_health::capacitor_snapshot(&f.db, &f.world, f.system, ship).await.map_err(error)?.is_some_and(|cap| cap.level < cap_before)
                    } else if kind == EffectKind::Ecm {
                        combat::jamming::is_jammed(ship, crate::space::now_ms()) && !combat::targeting_charges::targets_of(victim).contains(&primary.1)
                    } else {
                        let source = f.world.with(f.system, move |s| s.projections_on(ship).iter().any(|p| p.source_module == drone && p.source_ship == drone)).await.unwrap_or(false);
                        let fit = Fit::load_in_space(&f.db, &f.world, f.system, victim, ship).await.map_err(error)?;
                        source && baseline.iter().any(|&(key, attr, old)| {
                            let new = fit.value(key, attr);
                            if kind == (EffectKind::Projected { jamming: JammingType::Paint }) { new > old } else { new < old }
                        })
                    };
                    if done { break; }
                    ensure!(f.world.after(f.system, 100).await, "system stopped during {kind:?}");
                }
                Ok::<_, anyhow::Error>(())
            }).await.map_err(|_| anyhow!("{kind:?} did not affect receiver"))??;
            command(&mut f, "CmdReturnBay", &[Value::list([Value::Int(drone)])]).await?;
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let row = f.db.query_one("SELECT location_id,flag_id FROM game.items WHERE item_id=$1", &[&drone]).await?;
                    let returned = row.get::<_, i64>("location_id") == primary.1 && row.get::<_, i32>("flag_id") == FLAG_DRONE_BAY;
                    let clean = f.world.with(f.system, move |s| s.ball(drone).is_none() && !s.projections_on(ship).iter().any(|p| p.source_module == drone)).await.unwrap_or(false);
                    if returned && clean && !combat::jamming::is_jammed(ship, crate::space::now_ms()) { break; }
                    ensure!(f.world.after(f.system, 100).await, "system stopped returning {kind:?}");
                }
                Ok::<_, anyhow::Error>(())
            }).await.map_err(|_| anyhow!("{kind:?} return/cleanup timed out"))??;
        }
        Ok(())
    }).await;
    if let Some((owner, ship)) = receiver {
        f.client.session.apply(vec![("charid", Value::Int(owner)), ("shipid", Value::Int(ship))]);
        combat::reset(&f.client);
    }
    f.client.session.apply(vec![("charid", Value::Int(primary.0)), ("shipid", Value::Int(primary.1))]);
    f.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
