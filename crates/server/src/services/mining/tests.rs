use super::*;
use super::reprocessing_svc::{add_output, output_stack_quantity};
use crate::services::drones::tests::fixture::Fixture;

mod extraction;

fn fixed_intent(rock: i64, type_id: i32, unit_volume: f64, units: f64) -> crate::space::drone::PendingMining {
    crate::space::drone::PendingMining {
        rock, type_id, unit_volume,
        inputs: rounding::MiningInputs { clamp_factor: 1.0, volume: unit_volume * units, waste_volume_multiplier: 0.0, waste_probability: 0.0, crit_quantity_multiplier: 0.0, crit_probability: 0.0, efficiency: 1.0 },
        draws: rounding::MiningDraws { waste_check: 0, normal_round: 0.5, waste_round: 0.5, crit_check: 0.5, crit_round: 0.5 },
    }
}

#[test]
fn field_volume_sampling_uses_half_open_range() {
    crate::tests::init_config();
    let range = crate::config::game().mining.ore_rock_m3;
    assert_eq!(sample_range(range, 0.0), range[0]);
    assert_eq!(sample_range(range, 0.5), range[0] + (range[1] - range[0]) / 2.0);
    assert_eq!(sample_range([17.0, 17.0], 0.5), 17.0);
}

#[test]
fn regrowth_deadline_rounds_up_to_milliseconds() {
    let whole = crate::client::filetime_from_unix_ms(42);
    assert_eq!(unix_ms_from_filetime_ceil(whole), 42);
    assert_eq!(unix_ms_from_filetime_ceil(whole + 1), 43);
}

#[test]
fn ore_bands_follow_security() {
    assert!(field_groups(Field::Ore, 0.9, false).0.contains(&"Veldspar"));
    assert!(field_groups(Field::Ore, 0.3, false).0.contains(&"Jaspet"));
    assert!(field_groups(Field::Ore, -0.4, false).0.contains(&"Arkonor"));
    assert!(field_groups(Field::Gas, -1.0, true).0.iter().all(|g| g.starts_with("Fullerite")));
    assert!(rock_radius(Field::Ore, 25_000.0) < 10_000.0);
}

#[test]
fn ore_candidates_require_matching_group_category_and_exact_grade() {
    let names = ["Veldspar"];
    assert!(ore_candidate("Veldspar", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(ore_candidate("Veldspar II-Grade", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(ore_candidate("Veldspar III-Grade", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(!ore_candidate("Veldspar IV-Grade", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(!ore_candidate("Veldspar", "Veldspar", None, &names));
    assert!(!ore_candidate("Scordite", "Scordite", Some(CATEGORY_ASTEROID), &names));
}

#[test]
fn output_reuses_fully_consumed_source_quantity() {
    assert!(matches!(output_stack_quantity(None, 12), Ok(12)));
    assert!(matches!(output_stack_quantity(Some(3), 12), Ok(15)));
}

#[test]
fn output_overflow_is_a_typed_refusal() {
    for result in [
        add_output(i64::MAX, 1),
        output_stack_quantity(None, i64::from(i32::MAX) + 1).map(i64::from),
        output_stack_quantity(Some(i32::MAX), 1).map(i64::from),
    ] {
        assert!(matches!(result, Err(CallError::User { msg, .. }) if msg == "CustomNotify"));
    }
}


#[test]
fn extraction_caps_rock_and_never_wastes_when_full() {
    assert_eq!(extraction_amounts(10, 5, 12, 0), (0, 0));
    assert_eq!(extraction_amounts(10, 5, 12, 20), (10, 2));
    assert_eq!(extraction_amounts(10, 5, 3, 20), (3, 0));
    assert_eq!(extraction_amounts(10, 5, 20, 4), (4, 5));
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn delivery_outcomes_preserve_full_and_depleted_rows() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut client, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let (ore, volume, fits) = sde.types.iter()
        .filter_map(|ore| {
            let volume = ore.volume.filter(|v| v.is_finite() && *v >= 0.01)?;
            if ore.category_id != Some(CATEGORY_ASTEROID) { return None; }
            let find_fit = |flag| sde.types.iter()
                .filter(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP))
                .find_map(|hull| {
                    let fit = Fit::for_space_type(sde.clone(), hull.type_id, Vec::new());
                    let (actual_flag, capacity) = destination(&fit);
                    let count = (capacity / volume).floor();
                    if actual_flag != flag || !count.is_finite() || count < 1.0 || count >= i32::MAX as f64 - 2.0 {
                        return None;
                    }
                    let full_room = ((capacity - count * volume) / volume).floor();
                    let one_room = ((capacity - (count - 1.0) * volume) / volume).floor();
                    (full_room < 1.0 && one_room == 1.0).then_some(fit)
                });
            Some((ore, volume, [find_fit(FLAG_CARGO)?, find_fit(FLAG_MINING_HOLD)?]))
        })
        .next()
        .expect("SDE must provide an ore and cargo and mining-hold fits supporting full and one-unit boundaries");
    let rock = crate::dao::asteroids::insert(&tx, 103, 104, ore.type_id, 5, 1.0, [0.0; 3]).await.unwrap();
    let mut pending = fixed_intent(rock, ore.type_id, volume, 2.0);
    let baseline = crate::dao::mining_ledger::list(&tx, 101, i64::MIN, i64::MAX).await.unwrap().len();
    let mut delivered_count = 0;
    for (mining_hold, fit) in [false, true].into_iter().zip(fits) {
        let (_, capacity) = destination(&fit);
        let flag = if mining_hold { FLAG_MINING_HOLD } else { FLAG_CARGO };
        let ship = if mining_hold { 106 } else { 105 };
        let quantity = (capacity / volume).floor() as i32;
        let item = crate::dao::items::transfers::insert(&tx, ore.type_id, 101, ship, flag, quantity).await.unwrap();
        let mut outbox = crate::db::outbox::Outbox::default();
        assert!(matches!(deliver(&tx, &mut outbox, &sde, 101, ship, 103, 107, &pending, destination(&fit)).await.ok().unwrap(), DeliveryOutcome::Full { flag: f } if f == flag));
        assert!(outbox.into_entries().is_empty());
        assert_eq!(crate::dao::mining_ledger::list(&tx, 101, i64::MIN, i64::MAX).await.unwrap().len(), baseline + delivered_count);
        assert_eq!(crate::dao::asteroids::lock(&tx, 103, rock, ore.type_id).await.unwrap(), Some(5));
        tx.execute("UPDATE game.items SET quantity = quantity - 1 WHERE item_id = $1", &[&item]).await.unwrap();
        let mut outbox = crate::db::outbox::Outbox::default();
        assert!(matches!(deliver(&tx, &mut outbox, &sde, 101, ship, 103, 107, &pending, destination(&fit)).await.ok().unwrap(), DeliveryOutcome::Delivered(d) if !d.repeat()));
        assert_eq!(outbox.into_entries().len(), 2);
        delivered_count += 1;
        let logs = crate::dao::mining_ledger::list(&tx, 101, i64::MIN, i64::MAX).await.unwrap();
        assert_eq!(logs.len(), baseline + delivered_count);
        assert_eq!((logs[0].type_id, logs[0].solar_system_id, logs[0].quantity, logs[0].quantity_wasted), (ore.type_id, 103, 1, 0));
        tx.execute("UPDATE game.asteroids SET quantity = 5 WHERE asteroid_id = $1", &[&rock]).await.unwrap();
    }
    let hull = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP)).unwrap();
    let fit = Fit::for_space_type(sde.clone(), hull.type_id, Vec::new());
    let mut outbox = crate::db::outbox::Outbox::default();
    pending.inputs.volume = 0.0;
    assert!(matches!(deliver(&tx, &mut outbox, &sde, 101, 105, 103, 107, &pending, destination(&fit)).await.ok().unwrap(), DeliveryOutcome::NoYield));
    tx.execute("UPDATE game.asteroids SET quantity = 0 WHERE asteroid_id = $1", &[&rock]).await.unwrap();
    assert!(matches!(deliver(&tx, &mut outbox, &sde, 101, 105, 103, 107, &pending, destination(&fit)).await.ok().unwrap(), DeliveryOutcome::Depleted { rock: r } if r == rock));
    pending.rock = rock + 1_000_000_000;
    assert!(matches!(deliver(&tx, &mut outbox, &sde, 101, 105, 103, 107, &pending, destination(&fit)).await.ok().unwrap(), DeliveryOutcome::Missing));
    assert_eq!(crate::dao::mining_ledger::list(&tx, 101, i64::MIN, i64::MAX).await.unwrap().len(), baseline + delivered_count);
    assert!(outbox.into_entries().is_empty());
    tx.rollback().await.unwrap();
}

#[test]
fn full_destination_selects_existing_hold_notice() {
    assert_eq!(full_notice(FLAG_MINING_HOLD), messages::MINING_HOLD_FULL);
    assert_eq!(full_notice(FLAG_CARGO), messages::CARGO_HOLD_FULL);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn zero_row_retry_reconciles_suppressed_delivery_callback() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ore = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID) && t.volume.is_some_and(|v| v >= 0.01)).unwrap();
    let hull = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP)
        && t.attribute(ATTR_MINING_HOLD).unwrap_or(0.0).max(t.attribute(ATTR_CAPACITY).unwrap_or(0.0)) >= ore.volume.unwrap()).unwrap();
    let fit = Fit::for_space_type(sde.clone(), hull.type_id, Vec::new());
    let system = -crate::client::filetime_now();
    let ship = system + 1;
    let rock = crate::dao::asteroids::insert(&db, system, system, ore.type_id, 1, 1.0, [0.0; 3]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    state().rocks.insert(rock, Rock { system, field: system, type_id: ore.type_id, quantity: 1 });
    world.add_ship(system, rock_ball(rock, ore.type_id, [0.0; 3], 1.0)).await.unwrap();
    let other = system + 2;
    state().rocks.insert(other, Rock { system, field: system, type_id: ore.type_id, quantity: 10 });
    delivered(&db, &world, system, 101, Delivery { rock: other, left: 3, removed: 1, full: false }).await.ok().unwrap();
    delivered(&db, &world, system, 101, Delivery { rock: other, left: 7, removed: 1, full: false }).await.ok().unwrap();
    assert_eq!(state().rocks.get(&other).unwrap().quantity, 3);
    state().rocks.remove(&other);
    let pending = fixed_intent(rock, ore.type_id, ore.volume.unwrap(), 1.0);
    let mut outbox = crate::db::outbox::Outbox::default();
    assert!(matches!(deliver(&db, &mut outbox, &sde, 101, ship, system, 107, &pending, destination(&fit)).await.ok().unwrap(), DeliveryOutcome::Delivered(_)));
    assert_eq!(rock_type(rock, system), Some(ore.type_id));
    let mut retry_outbox = crate::db::outbox::Outbox::default();
    assert!(matches!(deliver(&db, &mut retry_outbox, &sde, 101, ship, system, 107, &pending, destination(&fit)).await.ok().unwrap(), DeliveryOutcome::Depleted { rock: r } if r == rock));
    reconcile_depleted(&db, &world, rock).await.ok().unwrap();
    assert!(rock_type(rock, system).is_none());
    assert!(crate::dao::asteroids::lock(&db, system, rock, ore.type_id).await.unwrap().is_none());
    assert!(retry_outbox.into_entries().is_empty());
    db.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1 AND solar_system_id=$2", &[&101_i64, &system]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE location_id = $1 AND owner_id = $2", &[&ship, &101_i64]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn laser_full_holds_notify_and_one_unit_still_extracts() {
    let f = Fixture::new().await;
    let ore = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID) && t.volume.is_some_and(|v| v >= 0.01)).unwrap();
    let ore_type = ore.type_id;
    let volume = ore.volume.unwrap();
    let module_type = f.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_MODULE)).unwrap().type_id;
    let module: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id",
        &[&module_type, &f.owner, &f.ship, &crate::consts::FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
    for (item, attr, value) in [(module, ATTR_MINING_AMOUNT, volume * 2.0), (module, ATTR_MAX_RANGE, 10000.0),
        (module, ATTR_CRIT_CHANCE, 0.0), (module, ATTR_WASTE_PROBABILITY, 0.0),
        (f.ship, ATTR_CAPACITY, volume * 3.5), (f.ship, ATTR_MINING_HOLD, 0.0)] {
        f.set(item, attr, value).await;
    }
    let rock = crate::dao::asteroids::insert(&f.db, f.system, f.system, ore_type, 10, 1.0, [0.0; 3]).await.unwrap();
    state().rocks.insert(rock, Rock { system: f.system, field: f.system, type_id: ore_type, quantity: 10 });
    f.world.add_ship(f.system, rock_ball(rock, ore_type, [0.0; 3], 1.0)).await.unwrap();
    let item = crate::dao::items::transfers::insert(&f.db, ore_type, f.owner, f.ship, FLAG_CARGO, 3).await.unwrap();
    let baseline = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap();
    let (notify, mut notices) = tokio::sync::mpsc::unbounded_channel();
    let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.ok().unwrap();
    assert!(!extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, crate::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap());
    assert!(format!("{:?}", notices.try_recv().unwrap()).contains(messages::CARGO_HOLD_FULL));
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(10));
    assert_eq!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap(), baseline);
    f.db.execute("UPDATE game.items SET quantity = 2 WHERE item_id = $1", &[&item]).await.unwrap();
    assert!(!extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, crate::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap());
    assert!(format!("{:?}", notices.try_recv().unwrap()).contains(messages::CARGO_HOLD_FULL));
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(9));
    let logs = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap();
    assert_eq!(logs.len(), baseline.len() + 1);
    assert_eq!((logs[0].type_id, logs[0].solar_system_id, logs[0].quantity, logs[0].quantity_wasted), (ore_type, f.system, 1, 0));
    f.set(f.ship, ATTR_MINING_HOLD, volume).await;
    let mining_stack = crate::dao::items::transfers::insert(&f.db, ore_type, f.owner, f.ship, FLAG_MINING_HOLD, 1).await.unwrap();
    let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.ok().unwrap();
    assert!(!extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, crate::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap());
    assert!(format!("{:?}", notices.try_recv().unwrap()).contains(messages::MINING_HOLD_FULL));
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(9));
    f.set(f.ship, ATTR_MINING_HOLD, volume * (i32::MAX as f64 + 2.0)).await;
    f.db.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&mining_stack, &i32::MAX]).await.unwrap();
    let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.ok().unwrap();
    assert!(!extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, crate::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap());
    assert!(format!("{:?}", notices.try_recv().unwrap()).contains(messages::MINING_HOLD_FULL));
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(9));
    assert_eq!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap(), logs);
    state().rocks.remove(&rock);
    crate::dao::asteroids::delete(&f.db, rock).await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn delivery_rollback_restores_all_resource_types_and_suppresses_dispatch() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new().await;
    let mut rocks = Vec::new();
    let result: anyhow::Result<()> = async {
        for field in [Field::Ore, Field::Ice, Field::Gas] {
            let (type_id, volume) = *field_types(&f.sde, field, 1.0, false).first().expect("SDE resource type");
            let rock = crate::dao::asteroids::insert(&f.db, f.system, f.system, type_id, 5, 1.0, [0.0; 3]).await?;
            rocks.push(rock);
            let before = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
            let mut pending = fixed_intent(rock, type_id, volume, 3.0);
            pending.inputs.waste_probability = 100.0;
            pending.inputs.waste_volume_multiplier = 10.0;
            let dispatched = AtomicBool::new(false);
            let sde = f.sde.clone();
            let owner = f.owner;
            let ship = f.ship;
            let system = f.system;
            let result: Result<(), CallError> = f.db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| {
                let sde = sde.clone();
                let pending = pending.clone();
                Box::pin(async move {
                    assert!(matches!(deliver(tx, outbox, &sde, owner, ship, system, 1, &pending, (FLAG_CARGO, volume * 10.0)).await?, DeliveryOutcome::Delivered(_)));
                    let logs = crate::dao::mining_ledger::list(tx, owner, i64::MIN, i64::MAX).await?;
                    assert_eq!((logs[0].type_id, logs[0].quantity, logs[0].quantity_wasted), (type_id, 3, 2));
                    Err(CallError::Internal(anyhow::anyhow!("injected rollback")))
                })
            }, |(), _outbox| async {
                dispatched.store(true, Ordering::SeqCst);
                Ok(())
            }).await;
            anyhow::ensure!(result.is_err(), "delivery transaction unexpectedly committed");
            anyhow::ensure!(!dispatched.load(Ordering::SeqCst), "rolled-back outbox was dispatched");
            anyhow::ensure!(crate::dao::asteroids::lock(&f.db, system, rock, type_id).await? == Some(5), "asteroid changed after rollback");
            anyhow::ensure!(crate::dao::mining_ledger::list(&f.db, owner, i64::MIN, i64::MAX).await? == before, "ledger changed after rollback");
            let count: i64 = f.db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3", &[&owner, &ship, &type_id]).await?.get("n");
            anyhow::ensure!(count == 0, "items changed after rollback");
        }
        Ok(())
    }.await;
    for rock in rocks { crate::dao::asteroids::delete(&f.db, rock).await?; }
    f.cleanup().await;
    result
}
