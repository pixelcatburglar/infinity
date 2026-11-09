use super::*;
use crate::{db::outbox::Outbox, lua::events::GameEvent};

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_resource_deliveries_create_exact_progress_slices() -> anyhow::Result<()> {
    let f = Fixture::new().await;
    let rocks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let owned_rocks = rocks.clone();
    let db = f.db.clone();
    let sde = f.sde.clone();
    let (owner, ship, system) = (f.owner, f.ship, f.system);
    let result = tokio::spawn(async move {
        let resources = [(Field::Ore, "ore", "ore"), (Field::Ice, "ice", "ore"), (Field::Gas, "gas", "gas")]
            .into_iter()
            .map(|(field, label, kind)| {
                let (type_id, volume) = *field_types(&sde, field, 1.0, false).first().expect("SDE resource type");
                (label, kind, type_id, volume)
            })
            .collect::<Vec<_>>();
        let capacity = resources.iter().map(|(_, _, _, volume)| volume * 10.0).sum::<f64>();
        let mut expected = Vec::new();
        for (label, kind, type_id, volume) in resources {
            let rock = crate::dao::asteroids::insert_batch(&db, system, &[NewAsteroid { field: system, type_id, quantity: 5, radius: 1.0, pos: [0.0; 3] }]).await?[0].id;
            owned_rocks.lock().unwrap().push(rock);
            let mut pending = fixed_intent(rock, type_id, volume, 3.0);
            pending.inputs.waste_probability = 100.0;
            pending.inputs.waste_volume_multiplier = 10.0;
            let delivery_sde = sde.clone();
            db.transaction(|tx, outbox: &mut Outbox| {
                let sde = delivery_sde.clone();
                let pending = pending.clone();
                Box::pin(async move {
                    let outcome = deliver(tx, outbox, &sde, owner, ship, system, 1, &pending, (FLAG_CARGO, capacity)).await?;
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
            assert_eq!((row.get::<_, i64>("quantity"), row.get::<_, i64>("quantity_wasted")), (3, 2), "{label} type {type_id} ledger quantities");
            assert!(!row.get::<_, bool>("progress_aggregated"), "{label} type {type_id} initial marker");
            expected.push((row.get::<_, i64>("log_id"), row.get::<_, i64>("event_time"), type_id, label, kind, volume));
        }
        db.transaction(|tx, outbox: &mut Outbox| Box::pin(super::super::progress::aggregate_tx(tx, outbox, 100)), |value, _| async move { Ok(value) }).await.map_err(|e| anyhow::anyhow!("aggregate: {e:?}"))?;
        for (id, time, type_id, label, kind, volume) in expected {
            let event_kind = if kind == "gas" { "GasHarvested" } else { "OreMined" };
            let events = crate::db::outbox::tests::events(&db, event_kind, owner).await;
            let slices = events.into_iter().filter_map(|event| match event {
                GameEvent::OreMined { ore_type, slices, .. } if kind == "ore" && ore_type == type_id => Some(slices),
                GameEvent::GasHarvested { gas_type, slices, .. } if kind == "gas" && gas_type == type_id => Some(slices),
                _ => None,
            }).flatten().filter(|slice| slice.log_id == id).collect::<Vec<_>>();
            assert_eq!(slices.len(), 1, "{label} type {type_id} ({kind}) event slices");
            assert_eq!(slices[0].quantity, 3, "{label} type {type_id} event quantity");
            assert_eq!(slices[0].occurred_at, time, "{label} type {type_id} event time");
            assert_eq!(slices[0].volume_m3, volume * 3.0, "{label} type {type_id} event volume");
            assert!(db.query_one("SELECT progress_aggregated FROM game.character_mining_ledger WHERE log_id=$1", &[&id]).await?.get::<_, bool>("progress_aggregated"), "{label} type {type_id} final marker");
        }
        Ok::<_, anyhow::Error>(())
    }).await;
    let owned_rocks = std::mem::take(&mut *rocks.lock().unwrap());
    let mut rock_cleanup = Ok(());
    for rock in owned_rocks {
        if let Err(error) = f.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &f.system, &f.system]).await {
            rock_cleanup = Err(error);
        }
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
    let before = crate::dao::mining_ledger::list(&f.db, owner, i64::MIN, i64::MAX).await?;
    let cargo: i64 = f.db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1 AND location_id=$2", &[&owner, &ship]).await?.get("count");
    for missing_ship in [false, true] {
        let pending = fixed_intent(rock, type_id, volume, 3.0);
        let sde = f.sde.clone();
        let result: Result<(), CallError> = f.db.transaction(|tx, outbox: &mut Outbox| {
            let pending = pending.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let removed = if missing_ship {
                    tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.expect("remove fixture ship")
                } else {
                    tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.expect("remove fixture character")
                };
                assert_eq!(removed, 1);
                deliver(tx, outbox, &sde, owner, ship, system, 1, &pending, (FLAG_CARGO, volume * 10.0)).await?;
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
