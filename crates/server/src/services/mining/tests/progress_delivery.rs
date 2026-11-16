use super::*;
use crate::{db::outbox::Outbox, lua::events::GameEvent};

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_resource_deliveries_create_exact_progress_slices() -> anyhow::Result<()> {
    let f = Fixture::real_system().await;
    let registration = crate::services::sovereignty::indices::progress::registration();
    let subscription_existed = f.db.query_opt("SELECT consumer_key FROM game.progress_subscriptions WHERE consumer_key=$1", &[&registration.consumer_key]).await?.is_some();
    assert!(crate::dao::sovereignty::development::read(&f.db, f.system).await?.is_none());
    let registry = crate::progress::tests::fixture_registry(&f.db, crate::progress::Registry::new(vec![], vec![registration]).unwrap()).await;
    let indices = crate::progress::tests::indices_host(&f.db, &f.sde, &f.world).await;
    let policies = indices.policies.clone();
    let world = f.world.clone();
    let rocks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let owned_rocks = rocks.clone();
    let db = f.db.clone().with_progress_registry(registry);
    let sde = f.sde.clone();
    let (owner, ship, system) = (f.owner, f.ship, f.system);
    let result = tokio::spawn(async move {
        let history = crate::services::sovereignty::indices::activation::load(&policies).unwrap();
        let definitions = &history.head().definitions;
        let band = &definitions.industrial[0];
        let next = &definitions.industrial[1];
        let factor = definitions.industrial_points_per_m3 * band.modifier;
        crate::space::sovereignty::indices::install_policy(&world, crate::services::sovereignty::indices::model::IndexCapability::Ready(history.clone()));
        let (notify_tx, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(owner, notify_tx);
        world.with(system, |_| ()).await.expect("pilot system");
        let (_, views) = crate::services::sovereignty::indices::query::read_views(&db, &sde, &policies, Some(&[system]), crate::client::filetime_now()).await.unwrap();
        assert_eq!((views[0].industrial.level, views[0].industrial_increasing), (0, false));
        let resources = [(Field::Ore, "ore", "ore"), (Field::Ice, "ice", "ore"), (Field::Gas, "gas", "gas")]
            .into_iter()
            .map(|(field, label, kind)| {
                let (type_id, volume) = *field_types(&sde, field, 1.0, false).iter()
                    .filter(|(_, volume)| volume.is_finite() && *volume > 0.0 && 3.0 * volume * factor < band.max_points / 8.0)
                    .max_by(|a, b| a.1.total_cmp(&b.1)).expect("SDE resource type below first band");
                let units = if kind == "gas" {
                    ((band.max_points + (next.max_points - next.min_points) / 4.0) / (volume * factor)).ceil() as i64
                } else { (band.max_points / (8.0 * volume * factor)).floor() as i64 };
                (label, kind, type_id, volume, units)
            })
            .collect::<Vec<_>>();
        let capacity = resources.iter().map(|(_, _, _, volume, units)| volume * (*units + 1) as f64).sum::<f64>();
        crate::dao::items::dogma::set_override(&db, ship, infinity_core::consts::dogma::ATTR_CAPACITY, capacity).await?;
        let foreign: i64 = db.query_one("SELECT count(*) AS count FROM game.character_mining_ledger WHERE NOT progress_aggregated AND character_id<>$1", &[&owner]).await?.get("count");
        assert_eq!(foreign, 0, "dedicated fixture requires no foreign pending mining ledger");
        for (stage, (label, kind, type_id, volume, units)) in resources.into_iter().enumerate() {
            let rock = crate::dao::asteroids::insert_batch(&db, system, &[NewAsteroid { field: system, type_id, quantity: units + 2, radius: 1.0, pos: [0.0; 3] }]).await?[0].id;
            owned_rocks.lock().unwrap().push(rock);
            let mut pending = fixed_intent(rock, type_id, volume, units as f64);
            pending.inputs.waste_probability = 100.0;
            pending.inputs.waste_volume_multiplier = 10.0;
            let delivery_sde = sde.clone();
            db.transaction(|tx, outbox: &mut Outbox| {
                let sde = delivery_sde.clone();
                let pending = pending.clone();
                Box::pin(async move {
                    let snapshot = crate::services::inventory::hold_storage::HoldCapacity::capture(
                        &crate::services::fit::Fit::load_unpiloted(tx, sde.clone(), ship).await?, FLAG_CARGO)?;
                    let outcome = deliver(tx, outbox, &sde, owner, system, 1, &pending, snapshot).await?;
                    let stage = match outcome {
                        DeliveryOutcome::Delivered(_) => "delivered",
                        DeliveryOutcome::Full { .. } => "full",
                        DeliveryOutcome::NoYield => "no yield",
                        DeliveryOutcome::Depleted { .. } => "depleted",
                        DeliveryOutcome::Missing => "missing",
                    };
                    assert_eq!(stage, "delivered", "{label} type {type_id} ({kind}) delivery outcome");
                    Ok::<_, CallError>(())
                })
            }, |value, _| async move { Ok(value) }).await.map_err(|e| anyhow::anyhow!("{label} type {type_id} ({kind}) delivery: {e:?}"))?;
            let row = db.query_one("SELECT log_id, event_time, resource_kind, quantity, quantity_wasted, progress_aggregated FROM game.character_mining_ledger WHERE character_id=$1 AND type_id=$2 ORDER BY log_id DESC LIMIT 1", &[&owner, &type_id]).await?;
            assert_eq!(row.get::<_, Option<String>>("resource_kind").as_deref(), Some(kind), "{label} type {type_id} ledger kind");
            assert_eq!((row.get::<_, i64>("quantity"), row.get::<_, i64>("quantity_wasted")), (units, 2), "{label} type {type_id} ledger quantities");
            assert!(!row.get::<_, bool>("progress_aggregated"), "{label} type {type_id} initial marker");
            let (id, time) = (row.get::<_, i64>("log_id"), row.get::<_, i64>("event_time"));
            db.transaction(|tx, outbox: &mut Outbox| Box::pin(super::super::progress::aggregate_tx(tx, outbox, 100)), |value, _| async move { Ok(value) }).await.map_err(|e| anyhow::anyhow!("aggregate: {e:?}"))?;
            let mut event_ids = Vec::new();
            let event_kind = if kind == "gas" { "GasHarvested" } else { "OreMined" };
            let key = crate::db::outbox::SourceKey::new(event_kind, id);
            if let Some(event) = crate::dao::progress::source(&db, "business", key.as_str()).await? {
                event_ids.push(event.event_id);
            }
            let events = crate::db::outbox::tests::events(&db, event_kind, owner).await;
            let slices = events.into_iter().filter_map(|event| match event {
                GameEvent::OreMined { ore_type, slices, .. } if kind == "ore" && ore_type == type_id => Some(slices),
                GameEvent::GasHarvested { gas_type, slices, .. } if kind == "gas" && gas_type == type_id => Some(slices),
                _ => None,
            }).flatten().filter(|slice| slice.log_id == id).collect::<Vec<_>>();
            assert_eq!(slices.len(), 1, "{label} type {type_id} ({kind}) event slices");
            assert_eq!(slices[0].quantity, units, "{label} type {type_id} event quantity");
            assert_eq!(slices[0].occurred_at, time, "{label} type {type_id} event time");
            assert_eq!(slices[0].volume_m3, volume * units as f64, "{label} type {type_id} event volume");
            assert!(db.query_one("SELECT progress_aggregated FROM game.character_mining_ledger WHERE log_id=$1", &[&id]).await?.get::<_, bool>("progress_aggregated"), "{label} type {type_id} final marker");
            assert!(!event_ids.is_empty());
            crate::progress::tests::drain_indices(&db, &sde, &world, policies.clone(), &event_ids).await;
            world.with(system, |_| ()).await.expect("pilot system");
            let (_, views) = crate::services::sovereignty::indices::query::read_views(&db, &sde, &policies, Some(&[system]), crate::client::filetime_now()).await.unwrap();
            assert_eq!((views[0].industrial.level, views[0].industrial_increasing), (if stage < 2 { 0 } else { 1 }, true));
            assert_eq!(std::iter::from_fn(|| notifications.try_recv().ok()).filter(|(name, _)| name == "OnSolarSystemDevIndexChanged").count(), if stage == 1 { 0 } else { 1 });
        }
        let row = crate::dao::sovereignty::development::read(&db, system).await?.expect("mining development row");
        assert!(row.industrial_points > 0.0);
        assert_eq!(row.military_points, 0.0);
        Ok::<_, anyhow::Error>(())
    }).await;
    let owned_rocks = std::mem::take(&mut *rocks.lock().unwrap());
    let mut rock_cleanup = Ok(());
    for rock in owned_rocks {
        if let Err(error) = f.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &f.system, &f.system]).await {
            rock_cleanup = Err(error);
        }
    }
    f.db.execute("DELETE FROM game.sov_development_checkpoints WHERE system_id=$1 AND activation_ordinal=$2", &[&f.system, &indices.activation.ordinal]).await?;
    f.db.execute("DELETE FROM game.sov_development WHERE system_id=$1 AND activation_ordinal=$2", &[&f.system, &indices.activation.ordinal]).await?;
    f.db.execute("DELETE FROM game.lua_content_activations WHERE ordinal=$1", &[&indices.activation.ordinal]).await?;
    f.db.execute("DELETE FROM game.lua_content_versions WHERE namespace=$1 AND version=$2", &[&indices.activation.namespace, &indices.activation.version]).await?;
    if !subscription_existed {
        f.db.execute("DELETE FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=$2)", &[&"sovereignty-indices", &f.owner]).await?;
        f.db.execute("DELETE FROM game.progress_subscription_kinds WHERE consumer_key=$1", &[&"sovereignty-indices"]).await?;
        f.db.execute("DELETE FROM game.progress_subscriptions WHERE consumer_key=$1", &[&"sovereignty-indices"]).await?;
    }
    f.cleanup().await;
    rock_cleanup?;
    result??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn missing_delivery_snapshot_rolls_back_rock_cargo_and_ledger() -> anyhow::Result<()> {
    let f = Fixture::new().await;
    let (type_id, volume) = *field_types(&f.sde, Field::Ore, 1.0, false).first().expect("SDE ore type");
    let rock = crate::dao::asteroids::insert_batch(&f.db, f.system, &[NewAsteroid { field: f.system, type_id, quantity: 5, radius: 1.0, pos: [0.0; 3] }]).await?[0].id;
    let (owner, ship, system) = (f.owner, f.ship, f.system);
    crate::dao::items::dogma::set_override(&f.db, ship, infinity_core::consts::dogma::ATTR_CAPACITY, volume * 10.0).await?;
    let before = crate::dao::mining_ledger::list(&f.db, owner, i64::MIN, i64::MAX).await?;
    let cargo: i64 = f.db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1 AND location_id=$2", &[&owner, &ship]).await?.get("count");
    for missing_ship in [false, true] {
        let pending = fixed_intent(rock, type_id, volume, 3.0);
        let sde = f.sde.clone();
        let result: Result<(), CallError> = f.db.transaction(|tx, outbox: &mut Outbox| {
            let pending = pending.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let snapshot = crate::services::inventory::hold_storage::HoldCapacity::capture(
                    &crate::services::fit::Fit::load_unpiloted(tx, sde.clone(), ship).await?, FLAG_CARGO)?;
                let removed = if missing_ship {
                    tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.expect("remove fixture ship")
                } else {
                    tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.expect("remove fixture character")
                };
                assert_eq!(removed, 1);
                deliver(tx, outbox, &sde, owner, system, 1, &pending, snapshot).await?;
                Ok(())
            })
        }, |value, _| async move { Ok(value) }).await;
        let error = result.unwrap_err();
        if missing_ship {
            let CallError::User { msg, dict } = &error else { panic!("expected missing ship user error, got {error:?}") };
            let CallError::User { msg: expected_msg, dict: expected_dict } =
                crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION)
            else { panic!("notify_error did not return a user error") };
            assert_eq!(msg, &expected_msg);
            assert_eq!(dict, &expected_dict);
        } else {
            let CallError::Internal(_) = &error else { panic!("expected missing character internal error, got {error:?}") };
            assert!(format!("{error:?}").contains("mining delivery source character or ship missing"), "{error:?}");
        }
        assert_eq!(crate::dao::asteroids::lock(&f.db, system, rock, type_id).await?, Some(5));
        assert_eq!(crate::dao::mining_ledger::list(&f.db, owner, i64::MIN, i64::MAX).await?, before);
        let after: i64 = f.db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1 AND location_id=$2", &[&owner, &ship]).await?.get("count");
        assert_eq!(after, cargo);
    }
    f.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &system, &system]).await?;
    f.cleanup().await;
    Ok(())
}
