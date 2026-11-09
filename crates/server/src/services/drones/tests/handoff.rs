use anyhow::{ensure, Context, Result};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{services::fit::{Fit, Key}};
use super::fixture::Fixture;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn launch_row_change_after_reservation_rolls_back_and_releases_quota() {
    let mut f = Fixture::new().await;
    let mut result: Result<()> = Ok(());
    let panic = crate::tests::catch_scenario(async {
        result = async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0))
            .context("published drone with bandwidth")?.type_id;
        f.train_drone(ty).await;
        let source = f.drone(ty, 2, false).await;
        let fit = Fit::drone(&f.db, f.sde.clone(), f.owner, source).await.map_err(|e| anyhow::anyhow!("fit: {e:?}"))?;
        let bandwidth = fit.value(Key::Drone(source), ATTR_DRONE_BANDWIDTH_USED);
        f.set(f.ship, ATTR_DRONE_BANDWIDTH, bandwidth * 5.0).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx);

        let cfg = crate::config::tests::test_database().to_pg_config()?;
        let (mut connection, driver) = cfg.connect(tokio_postgres::NoTls).await?;
        tokio::spawn(async move { let _ = driver.await; });
        let transaction = connection.transaction().await?;
        transaction.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&source]).await?;
        let world = f.world.clone();
        let owner = f.owner; let ship = f.ship; let system = f.system;
        let adversary = async move {
            let observed = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                loop {
                    let reserved = world.with(system, move |s| {
                        let probe = s.reserve_drone_launch(owner, ship, &[0.0; 5], 5.0, 1_000_000.0);
                        if let Some(token) = probe { s.cancel_drone_launch(token); }
                        probe.is_none()
                    }).await.context("system stopped")?;
                    if reserved { return Ok::<(), anyhow::Error>(()); }
                    tokio::task::yield_now().await;
                }
            }).await.context("reservation not observed")?;
            if observed.is_ok() {
                transaction.execute("UPDATE game.items SET quantity=3 WHERE item_id=$1", &[&source]).await?;
                transaction.commit().await?;
            } else {
                transaction.rollback().await?;
            }
            observed
        };
        let args = [Value::list([Value::tuple([Value::Int(source), Value::Int(1)])]), Value::None, Value::Bool(false)];
        let (launched, changed) = tokio::join!(f.call("ship", "LaunchDrones", &args), adversary);
        changed?;
        let Value::Dict(rows) = launched.map_err(|e| anyhow::anyhow!("launch: {e:?}"))? else { anyhow::bail!("launch result not mapping") };
        ensure!(rows.len() == 1 && rows[0].0 == Value::Int(source), "missing source result");
        ensure!(matches!(&rows[0].1, Value::List(values) if matches!(values.first(), Some(Value::Tuple(_)))), "launch did not refuse changed source");
        let row = f.db.query_one("SELECT location_id,flag_id,quantity,singleton FROM game.items WHERE item_id=$1", &[&source]).await?;
        ensure!(row.get::<_, i64>("location_id") == ship && row.get::<_, i32>("flag_id") == FLAG_DRONE_BAY && row.get::<_, i64>("quantity") == 3 && !row.get::<_, bool>("singleton"), "adversarial row changed");
        let count: i64 = f.db.query_one("SELECT count(*) FROM game.items WHERE owner_id=$1 AND location_id=$2 AND item_id<>$3", &[&owner, &system, &ship]).await?.get("count");
        ensure!(count == 0, "positioned item created on rollback");
        let (balls, probe) = f.world.with(system, move |s| {
            let balls = s.drones().count();
            let probe = s.reserve_drone_launch(owner, ship, &[0.0; 5], 5.0, 1_000_000.0);
            if let Some(token) = probe { s.cancel_drone_launch(token); }
            (balls, probe.is_some())
        }).await.context("system stopped")?;
        ensure!(balls == 0 && probe, "ball created or reservation leaked");
        ensure!(rx.try_recv().is_err(), "rollback emitted a pilot notice");
        Ok(())
        }.await;
    }).await;
    f.cleanup().await;
    if let Err(panic) = panic { std::panic::resume_unwind(panic); }
    result.expect("launch rollback regression");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn accepted_return_survives_controller_loss_and_removes_bay_ball() {
    let mut f = Fixture::new().await;
    let mut result: Result<()> = Ok(());
    let panic = crate::tests::catch_scenario(async {
        result = async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0))
            .context("published drone with bandwidth")?.type_id;
        f.train_drone(ty).await;
        let id = f.drone(ty, 1, true).await;
        f.set(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0).await;
        let launch = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
        let launched = f.call("ship", "LaunchDrones", &launch).await.map_err(|e| anyhow::anyhow!("launch: {e:?}"))?;
        ensure!(launched == Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]), "launch did not commit");
        let guard = crate::services::market::lock().await;
        let owner = f.owner; let ship = f.ship; let system = f.system;
        ensure!(f.world.with(system, move |s| s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay)).await.context("system stopped")?, "return order refused");
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let accepted = f.world.with(system, move |s| s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay)).await.context("system stopped")?;
                if !accepted { return Ok::<(), anyhow::Error>(()); }
            }
        }).await.context("contact claim not observed")??;
        let blocked = f.world.with(system, move |s| {
            s.drone_controller_lost(ship);
            !s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay)
                && s.drone(id).is_some_and(|d| d.controller.is_none())
        }).await.context("system stopped")?;
        drop(guard);
        ensure!(blocked, "claim failed to remain exclusive after controller loss");
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let row = f.db.query_one("SELECT location_id,flag_id FROM game.items WHERE item_id=$1", &[&id]).await?;
                if row.get::<_, i64>("location_id") == ship && row.get::<_, i32>("flag_id") == FLAG_DRONE_BAY
                    && f.world.ball(system, id).await.is_none() { return Ok::<(), anyhow::Error>(()); }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.context("committed return did not remove ball")??;
        ensure!(f.world.with(system, move |s| s.drone(id).is_none() && s.ball(id).is_none()).await.context("system stopped")?, "committed return left actor state");
        Ok(())
        }.await;
    }).await;
    f.cleanup().await;
    if let Err(panic) = panic { std::panic::resume_unwind(panic); }
    result.expect("claimed return regression");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn failed_claimed_return_releases_fence_after_controller_loss() {
    let mut f = Fixture::new().await;
    let mut result: Result<()> = Ok(());
    let panic = crate::tests::catch_scenario(async {
        result = async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)
            && t.volume.is_some_and(|v| v > 0.0)).context("published drone with positive volume")?.type_id;
        f.train_drone(ty).await;
        let id = f.drone(ty, 1, true).await;
        f.set(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        let launch = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
        ensure!(f.call("ship", "LaunchDrones", &launch).await.map_err(|e| anyhow::anyhow!("launch: {e:?}"))?
            == Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]), "launch did not commit");
        let original_generation = f.world.with(f.system, move |s| s.combat_membership_generation(id)).await.context("system stopped")?.context("missing drone condition")?;
        f.set(f.ship, ATTR_DRONE_CAPACITY, 0.0).await;
        let guard = crate::services::market::lock().await;
        let owner = f.owner; let ship = f.ship; let system = f.system;
        ensure!(f.world.with(system, move |s| s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay)).await.context("system stopped")?, "return order refused");
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                if !f.world.with(system, move |s| s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay)).await.context("system stopped")? {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }).await.context("contact claim not observed")??;
        let blocked = f.world.with(system, move |s| {
            s.drone_controller_lost(ship);
            !s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay)
        }).await.context("system stopped")?;
        drop(guard);
        ensure!(blocked, "claim did not survive controller loss");
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let released = f.world.with(system, move |s| {
                    if s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay) {
                        s.drone_controller_lost(ship);
                        true
                    } else { false }
                }).await.context("system stopped")?;
                if released { return Ok::<(), anyhow::Error>(()); }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.context("failed contact did not release claim")??;
        let row = f.db.query_one("SELECT location_id,owner_id,flag_id FROM game.items WHERE item_id=$1", &[&id]).await?;
        ensure!(row.get::<_, i64>("location_id") == system && row.get::<_, i64>("owner_id") == owner && row.get::<_, i32>("flag_id") == 0, "failed return changed inventory");
        ensure!(f.world.with(system, move |s| s.combat_membership_generation(id) == Some(original_generation)
            && s.combat_phase(id) == Some(crate::space::combat::CombatPhase::Live)).await.context("system stopped")?, "failed return did not restore matching live condition");
        ensure!(f.world.with(system, move |s| s.ball(id).is_some() && s.drone(id).is_some_and(|d| d.controller.is_none())).await.context("system stopped")?, "failed return lost drone");
        Ok(())
        }.await;
    }).await;
    f.cleanup().await;
    if let Err(panic) = panic { std::panic::resume_unwind(panic); }
    result.expect("failed claimed return regression");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn return_reconciles_a_drone_already_moved_by_another_transaction() {
    moved_drone_reconciliation(false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn reconciliation_does_not_retire_replacement_membership() {
    moved_drone_reconciliation(true).await;
}

async fn moved_drone_reconciliation(replace: bool) {
    let mut f = Fixture::new().await;
    let mut result: Result<()> = Ok(());
    let panic = crate::tests::catch_scenario(async {
        result = async {
            let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
                && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).context("published drone")?.type_id;
            f.train_drone(ty).await;
            let id = f.drone(ty, 1, true).await;
            f.set(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
            f.set(f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0).await;
            let launch = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
            ensure!(f.call("ship", "LaunchDrones", &launch).await.map_err(|e| anyhow::anyhow!("launch: {e:?}"))?
                == Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]), "launch refused");
            let (owner, ship, system) = (f.owner, f.ship, f.system);
            let cfg = crate::config::tests::test_database().to_pg_config()?;
            let (mut connection, driver) = cfg.connect(tokio_postgres::NoTls).await?;
            tokio::spawn(async move { let _ = driver.await; });
            let transaction = connection.transaction().await?;
            transaction.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&id]).await?;
            ensure!(f.world.with(system, move |s| s.drone_order(id, owner, ship, crate::space::drone::DroneOrder::ReturnBay)).await.context("system stopped")?, "return refused");
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                loop {
                    if f.world.with(system, move |s| matches!(s.combat_phase(id), Some(crate::space::combat::CombatPhase::Transition { .. }))).await.context("system stopped")? { return Ok::<(), anyhow::Error>(()); }
                    tokio::task::yield_now().await;
                }
            }).await.context("return transition not admitted")??;
            let newer_session = if replace {
                let mut session = infinity_core::session::OnlineSession::from_session(&f.client.session).unwrap();
                session.session_id = session.session_id.saturating_add(1);
                f.world.set_online_session(owner, Some(session.clone()));
                Some(session)
            } else { None };
            let replacement = if replace {
                let entries = crate::services::drones::inventory::restore_specs(&f.db, f.sde.clone(), system).await.ok().unwrap();
                let effects = crate::services::drones::worker::effects(f.db.clone(), std::sync::Arc::downgrade(&f.world), system);
                Some(f.world.with(system, move |s| {
                    let old = s.combat_membership_generation(id).unwrap();
                    s.remove_ball(id).unwrap();
                    for prepared in entries { s.restore_drone(prepared, effects.clone()); }
                    let new = s.combat_membership_generation(id).unwrap();
                    assert_ne!(old, new);
                    new
                }).await.context("system stopped replacing drone")?)
            } else { None };
            ensure!(crate::dao::items::drones::return_to_bay(&transaction, id, owner, system, owner, ship).await?, "external move failed");
            transaction.commit().await?;
            if let Some(generation) = replacement {
                let delay = crate::space::seconds_to_ms((crate::config::game().space.position_snapshot_secs + 2) as f64);
                ensure!(f.world.after(system, delay).await, "system stopped after reconciliation");
                ensure!(f.world.with(system, move |s| s.ball(id).is_some()
                    && s.combat_membership_generation(id) == Some(generation)
                    && s.combat_phase(id) == Some(crate::space::combat::CombatPhase::Live)).await.context("system stopped")?, "reconciliation retired replacement membership");
                ensure!(f.world.online_session(owner) == newer_session, "reconciliation changed newer session");
            } else {
                tokio::time::timeout(std::time::Duration::from_secs(60), async {
                    loop {
                        if f.world.ball(system, id).await.is_none() { return Ok::<(), anyhow::Error>(()); }
                        ensure!(f.world.after(system, 100).await, "system stopped");
                    }
                }).await.context("committed move was not reconciled")??;
            }
            let row = f.db.query_one("SELECT location_id,flag_id FROM game.items WHERE item_id=$1", &[&id]).await?;
            ensure!(row.get::<_, i64>("location_id") == ship && row.get::<_, i32>("flag_id") == FLAG_DRONE_BAY, "reconciliation changed the committed move");
            Ok(())
        }.await;
    }).await;
    f.cleanup().await;
    if let Err(panic) = panic { std::panic::resume_unwind(panic); }
    result.expect("moved drone reconciliation");
}
