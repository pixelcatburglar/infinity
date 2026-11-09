use super::*;
use super::reprocessing_svc::add_output;
use crate::services::drones::tests::fixture::Fixture;
use infinity_core::consts::{dogma::{ATTR_CAPACITY, ATTR_GENERAL_MINING_HOLD_CAPACITY}, FLAG_GENERAL_MINING_HOLD};
use infinity_core::consts::CATEGORY_ASTEROID;
use crate::dao::asteroids::NewAsteroid;
use crate::space::asteroids::generation::field_types;

pub(super) async fn publish_fixture(db: &Db, world: &World, system: i64, field: i64) {
    let actor = world.asteroid_actor(system).expect("system actor");
    let inputs = Arc::new(generation_inputs(world.sde(), [0.0; 3], Field::Ore, 1.0, false));
    let now = filetime_now();
    let prepared = db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
        |tx, _| {
            let inputs = Arc::clone(&inputs);
            Box::pin(async move { asteroids::prepare_field(tx, system, field, now, &inputs, false).await })
        },
        |batch, _| async move { Ok(batch) },
    ).await.unwrap();
    assert!(asteroids::publish(world, system, &actor, prepared).await);
}

mod extraction;
mod progress_delivery;

fn fixed_intent(rock: i64, type_id: i32, unit_volume: f64, units: f64) -> crate::space::drone::PendingMining {
    crate::space::drone::PendingMining {
        rock, type_id, unit_volume,
        inputs: rounding::MiningInputs { clamp_factor: 1.0, volume: unit_volume * units, waste_volume_multiplier: 0.0, waste_probability: 0.0, crit_quantity_multiplier: 0.0, crit_probability: 0.0, efficiency: 1.0 },
        draws: rounding::MiningDraws { waste_check: 0, normal_round: 0.5, waste_round: 0.5, crit_check: 0.5, crit_round: 0.5 },
    }
}


#[test]
fn output_quantity_supports_wide_values_and_refuses_overflow() {
    for value in [i64::from(i32::MAX) + 1, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
        assert_eq!(add_output(0, value).unwrap(), value);
    }
    assert!(matches!(add_output(i64::MAX, 1), Err(CallError::User { msg, .. }) if msg == "CustomNotify"));
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
    let fixture = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
    let owner = fixture.owner;
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut client, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let (ore, volume, fits) = sde.types.iter()
        .filter_map(|ore| {
            let volume = ore.volume.filter(|v| v.is_finite() && *v >= 0.01)?;
            if ore.category_id != Some(CATEGORY_ASTEROID) || !crate::services::inventory::holds::accepts(&sde, FLAG_GENERAL_MINING_HOLD, ore.type_id) { return None; }
            let find_fit = |flag| sde.types.iter()
                .filter(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP))
                .find_map(|hull| {
                    let fit = Fit::for_space_type(sde.clone(), hull.type_id, Vec::new());
                    let (actual_flag, capacity) = destination(&fit, ore.type_id);
                    let count = (capacity / volume).floor();
                    if actual_flag != flag || !count.is_finite() || count < 1.0 || count >= i32::MAX as f64 - 2.0 {
                        return None;
                    }
                    let full_room = ((capacity - count * volume) / volume).floor();
                    let one_room = ((capacity - (count - 1.0) * volume) / volume).floor();
                    (full_room < 1.0 && one_room == 1.0).then_some(fit)
                });
            Some((ore, volume, [find_fit(FLAG_CARGO)?, find_fit(FLAG_GENERAL_MINING_HOLD)?]))
        })
        .next()
        .expect("SDE must provide an ore and cargo and mining-hold fits supporting full and one-unit boundaries");
    let rock = crate::dao::asteroids::insert_batch(&tx, 103, &[NewAsteroid { field: 104, type_id: ore.type_id, quantity: 5, radius: 1.0, pos: [0.0; 3] }]).await.unwrap()[0].id;
    let mut pending = fixed_intent(rock, ore.type_id, volume, 2.0);
    let baseline = crate::dao::mining_ledger::list(&tx, owner, i64::MIN, i64::MAX).await.unwrap().len();
    let mut delivered_count = 0;
    let mut cargo_ship = 0;
    for (mining_hold, fit) in [false, true].into_iter().zip(fits) {
        let (_, capacity) = destination(&fit, ore.type_id);
        let flag = if mining_hold { FLAG_GENERAL_MINING_HOLD } else { FLAG_CARGO };
        let ship: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,0,1,true) RETURNING item_id", &[&fit.source_type(Key::Ship).unwrap(), &owner, &103_i64]).await.unwrap().get("item_id");
        if !mining_hold { cargo_ship = ship; }
        let quantity = (capacity / volume).floor() as i64;
        let item = crate::dao::items::transfers::insert(&tx, ore.type_id, owner, ship, flag, quantity).await.unwrap();
        let mut outbox = crate::db::outbox::Outbox::default();
        assert!(matches!(deliver(&tx, &mut outbox, &sde, owner, ship, 103, 107, &pending, destination(&fit, ore.type_id)).await.ok().unwrap(), DeliveryOutcome::Full { flag: f } if f == flag));
        assert!(outbox.into_entries().is_empty());
        assert_eq!(crate::dao::mining_ledger::list(&tx, owner, i64::MIN, i64::MAX).await.unwrap().len(), baseline + delivered_count);
        assert_eq!(crate::dao::asteroids::lock(&tx, 103, rock, ore.type_id).await.unwrap(), Some(5));
        assert!(deliver(&tx, &mut crate::db::outbox::Outbox::default(), &sde, owner, ship, 103, 107, &pending, (infinity_core::consts::FLAG_DRONE_BAY, capacity)).await.is_err());
        assert_eq!(crate::dao::asteroids::lock(&tx, 103, rock, ore.type_id).await.unwrap(), Some(5));
        tx.execute("UPDATE game.items SET quantity = quantity - 1 WHERE item_id = $1", &[&item]).await.unwrap();
        let mut outbox = crate::db::outbox::Outbox::default();
        assert!(matches!(deliver(&tx, &mut outbox, &sde, owner, ship, 103, 107, &pending, destination(&fit, ore.type_id)).await.ok().unwrap(), DeliveryOutcome::Delivered(d) if !d.repeat()));
        assert_eq!(outbox.into_entries().len(), 2);
        delivered_count += 1;
        let logs = crate::dao::mining_ledger::list(&tx, owner, i64::MIN, i64::MAX).await.unwrap();
        assert_eq!(logs.len(), baseline + delivered_count);
        assert_eq!((logs[0].type_id, logs[0].solar_system_id, logs[0].quantity, logs[0].quantity_wasted), (ore.type_id, 103, 1, 0));
        tx.execute("UPDATE game.asteroids SET quantity = 5 WHERE asteroid_id = $1", &[&rock]).await.unwrap();
    }
    tx.execute("UPDATE game.items SET quantity = $1 WHERE location_id = $2 AND type_id = $3 AND flag_id = $4", &[&(i64::MAX - 3), &cargo_ship, &ore.type_id, &FLAG_CARGO]).await.unwrap();
    tx.execute("UPDATE game.asteroids SET quantity = 10 WHERE asteroid_id = $1", &[&rock]).await.unwrap();
    pending.inputs.volume = volume * 10.0;
    let mut split_notices = crate::db::outbox::Outbox::default();
    assert!(matches!(deliver(&tx, &mut split_notices, &sde, owner, cargo_ship, 103, 107, &pending, (FLAG_CARGO, volume * (i64::MAX as f64 + 4096.0))).await.unwrap(), DeliveryOutcome::Delivered(_)));
    let stacks = crate::services::items::load_items(&tx, &sde, crate::dao::items::loading::ItemQuery::LocationFlagTypeOwnerStack(cargo_ship, FLAG_CARGO, ore.type_id, owner)).await.unwrap();
    assert_eq!(stacks.len(), 2);
    assert_eq!(stacks.iter().map(|item| i128::from(item.quantity)).sum::<i128>(), i128::from(i64::MAX) + 7);
    assert_eq!(split_notices.into_entries().len(), 3);
    assert_eq!(crate::dao::mining_ledger::list(&tx, owner, i64::MIN, i64::MAX).await.unwrap().len(), baseline + delivered_count + 1);
    delivered_count += 1;
    for stack in &stacks {
        tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&stack.item_id]).await.unwrap();
    }
    tx.execute("UPDATE game.asteroids SET quantity = 5 WHERE asteroid_id = $1", &[&rock]).await.unwrap();
    let hull = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap();
    let fit = Fit::for_space_type(sde.clone(), hull.type_id, Vec::new());
    let mut outbox = crate::db::outbox::Outbox::default();
    pending.inputs.volume = 0.0;
    assert!(matches!(deliver(&tx, &mut outbox, &sde, owner, cargo_ship, 103, 107, &pending, destination(&fit, ore.type_id)).await.ok().unwrap(), DeliveryOutcome::NoYield));
    tx.execute("UPDATE game.asteroids SET quantity = 0 WHERE asteroid_id = $1", &[&rock]).await.unwrap();
    assert!(matches!(deliver(&tx, &mut outbox, &sde, owner, cargo_ship, 103, 107, &pending, destination(&fit, ore.type_id)).await.ok().unwrap(), DeliveryOutcome::Depleted { rock: r } if r == rock));
    pending.rock = rock + 1_000_000_000;
    assert!(matches!(deliver(&tx, &mut outbox, &sde, owner, cargo_ship, 103, 107, &pending, destination(&fit, ore.type_id)).await.ok().unwrap(), DeliveryOutcome::Missing));
    assert_eq!(crate::dao::mining_ledger::list(&tx, owner, i64::MIN, i64::MAX).await.unwrap().len(), baseline + delivered_count);
    assert!(outbox.into_entries().is_empty());
    tx.rollback().await.unwrap();
    }).await;
    fixture.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[test]
fn full_destination_selects_existing_hold_notice() {
    assert_eq!(full_notice(FLAG_GENERAL_MINING_HOLD), messages::MINING_HOLD_FULL);
    assert_eq!(full_notice(FLAG_CARGO), messages::CARGO_HOLD_FULL);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn zero_row_retry_reconciles_suppressed_delivery_callback() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ore = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID) && t.volume.is_some_and(|v| v >= 0.01)).unwrap();
    let hull = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && t.attribute(ATTR_GENERAL_MINING_HOLD_CAPACITY).unwrap_or(0.0).max(t.attribute(ATTR_CAPACITY).unwrap_or(0.0)) >= ore.volume.unwrap()).unwrap();
    let fit = Fit::for_space_type(sde.clone(), hull.type_id, Vec::new());
    let system = -crate::client::filetime_now();
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,0,1,true) RETURNING item_id", &[&hull.type_id, &owner, &system]).await.unwrap().get("item_id");
    let rock = crate::dao::asteroids::insert_batch(&db, system, &[NewAsteroid { field: system, type_id: ore.type_id, quantity: 1, radius: 1.0, pos: [0.0; 3] }]).await.unwrap()[0].id;
    let world = World::new(db.clone(), sde.clone(), 1);
    world.system(system).await.unwrap();
    publish_fixture(&db, &world, system, system).await;
    let ore_type = ore.type_id;
    let volume = ore.volume.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let _market = super::super::market::lock().await;
        let pending = fixed_intent(rock, ore_type, volume, 1.0);
        let mut outbox = crate::db::outbox::Outbox::default();
        assert!(matches!(deliver(&db, &mut outbox, &sde, owner, ship, system, 107, &pending, destination(&fit, ore_type)).await.unwrap(), DeliveryOutcome::Delivered(_)));
        assert_eq!(world.with(system, move |s| s.rock_type(rock)).await.flatten(), Some(ore_type));
        let mut retry_outbox = crate::db::outbox::Outbox::default();
        assert!(matches!(deliver(&db, &mut retry_outbox, &sde, owner, ship, system, 107, &pending, destination(&fit, ore_type)).await.unwrap(), DeliveryOutcome::Depleted { rock: r } if r == rock));
        reconcile_depleted(&db, &world, system, rock).await.unwrap();
        assert!(world.with(system, move |s| s.rock_type(rock)).await.flatten().is_none());
        assert!(crate::dao::asteroids::lock(&db, system, rock, ore_type).await.unwrap().is_none());
        assert!(retry_outbox.into_entries().is_empty());
    }).await;
    db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &system, &system]).await.unwrap();
    db.execute("DELETE FROM game.asteroid_fields WHERE field_id=$1 AND system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1 AND solar_system_id=$2", &[&owner, &system]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE location_id = $1 AND owner_id = $2", &[&ship, &owner]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn reordered_committed_projections_never_increase_quantity() {
    let f = Fixture::new().await;
    let ore = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID)).unwrap().type_id;
    let rock = crate::dao::asteroids::insert_batch(&f.db, f.system, &[NewAsteroid { field: f.system, type_id: ore, quantity: 10, radius: 1.0, pos: [0.0; 3] }]).await.unwrap()[0].id;
    publish_fixture(&f.db, &f.world, f.system, f.system).await;
    delivered(&f.db, &f.world, f.system, f.owner, Delivery { rock, left: 3, removed: 7, full: false }).await.unwrap();
    delivered(&f.db, &f.world, f.system, f.owner, Delivery { rock, left: 7, removed: 3, full: false }).await.unwrap();
    let ship = f.ship;
    let rows = f.world.with(f.system, move |s| s.rocks_in_range(ship, 1_000_000.0)).await.flatten().unwrap();
    assert!(rows.contains(&(rock, ore, 3)));
    let actor = f.world.asteroid_actor(f.system).expect("fixture asteroid actor");
    f.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &f.system, &f.system]).await.unwrap();
    assert!(crate::space::asteroids::publish_depletion(&f.world, f.system, &actor, rock, f.system, None, None).await);
    f.db.execute("DELETE FROM game.asteroid_fields WHERE field_id=$1 AND system_id=$1", &[&f.system]).await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn committed_depletion_replay_removes_stale_actor_rock() {
    let f = Fixture::new().await;
    let ore = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID)).unwrap().type_id;
    let rock = crate::dao::asteroids::insert_batch(&f.db, f.system, &[NewAsteroid { field: f.system, type_id: ore, quantity: 1, radius: 1.0, pos: [0.0; 3] }]).await.unwrap()[0].id;
    publish_fixture(&f.db, &f.world, f.system, f.system).await;
    f.db.execute("UPDATE game.asteroids SET quantity=0 WHERE asteroid_id=$1", &[&rock]).await.unwrap();
    let _market = super::super::market::lock().await;
    let system = f.system;
    let now = filetime_now();
    let result = f.db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
        |tx, _| Box::pin(async move { crate::dao::asteroids::deplete_scoped(tx, system, system, rock, now, None).await.map_err(db_err) }),
        |result, _| async move { Ok(result) },
    ).await.unwrap();
    assert!(matches!(result, crate::dao::asteroids::Depletion::Deleted { .. }));
    assert_eq!(f.world.with(f.system, move |s| s.rock_type(rock)).await.flatten(), Some(ore));
    reconcile_depleted(&f.db, &f.world, f.system, rock).await.unwrap();
    assert!(f.world.with(f.system, move |s| s.rock_field(rock)).await.flatten().is_none());
    drop(_market);
    f.db.execute("DELETE FROM game.asteroid_fields WHERE field_id=$1 AND system_id=$1", &[&f.system]).await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn laser_full_holds_notify_and_one_unit_still_extracts() {
    let f = Fixture::new().await;
    let mut created_rock = None;
    let scenario = crate::tests::catch_scenario(async {
    let ship_type = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && t.attribute(ATTR_GENERAL_MINING_HOLD_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && crate::services::inventory::holds::effective_capacity(
            &Fit::for_space_type(f.sde.clone(), t.type_id, Vec::new()), infinity_core::consts::FLAG_SPECIAL_ASTEROID_HOLD) == 0.0)
        .expect("published ship with a general mining hold").type_id;
    f.db.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&f.ship, &ship_type]).await.unwrap();
    let mut ball = f.world.remove_ball(f.system, f.ship).await.expect("fixture ship ball");
    ball.type_id = ship_type;
    f.world.add_ship(f.system, ball).await.unwrap();
    let (ore_type, volume) = f.sde.types.iter().find_map(|t| {
        let volume = t.volume.filter(|v| v.is_finite() && *v >= 0.01)?;
        if t.category_id != Some(CATEGORY_ASTEROID)
            || t.group_id.is_some_and(|g| [infinity_core::consts::GROUP_ICE, infinity_core::consts::GROUP_TEMPORAL_RESOURCES,
                infinity_core::consts::GROUP_ANCIENT_COMPRESSED_ICE, infinity_core::consts::GROUP_HARVESTABLE_CLOUD,
                infinity_core::consts::GROUP_COMPRESSED_GAS].contains(&g))
            || !crate::services::inventory::holds::accepts(&f.sde, FLAG_GENERAL_MINING_HOLD, t.type_id) {
            return None;
        }
        let item_volume = crate::services::item_volume::item_volume(&f.sde, t.type_id, false, 1, None).ok()?;
        (item_volume == volume).then_some((t.type_id, volume))
    }).expect("asteroid ore accepted by the general mining hold with positive stack volume");
    let module_type = f.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)).unwrap().type_id;
    let module: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id",
        &[&module_type, &f.owner, &f.ship, &infinity_core::consts::FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
    for (item, attr, value) in [(module, ATTR_MINING_AMOUNT, volume * 2.0), (module, ATTR_MAX_RANGE, 10000.0),
        (module, ATTR_CRIT_CHANCE, 0.0), (module, ATTR_WASTE_PROBABILITY, 0.0),
        (f.ship, ATTR_CAPACITY, volume * 3.5), (f.ship, ATTR_GENERAL_MINING_HOLD_CAPACITY, 0.0)] {
        f.set(item, attr, value).await;
    }
    let rock = crate::dao::asteroids::insert_batch(&f.db, f.system, &[NewAsteroid { field: f.system, type_id: ore_type, quantity: 10, radius: 1.0, pos: [0.0; 3] }]).await.unwrap()[0].id;
    created_rock = Some(rock);
    publish_fixture(&f.db, &f.world, f.system, f.system).await;
    let item = crate::dao::items::transfers::insert(&f.db, ore_type, f.owner, f.ship, FLAG_CARGO, 3).await.unwrap();
    let baseline = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap();
    let (notify, mut notices) = tokio::sync::mpsc::unbounded_channel();
    let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.ok().unwrap();
    assert_eq!(destination(&fit, ore_type).0, FLAG_CARGO, "zero mining-hold override must route to cargo: {:?}", destination(&fit, ore_type));
    let result = extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, infinity_core::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap();
    assert!(!result.repeat && result.outcome == Some(crate::lua::events::ModuleCycleOutcome::CargoFull));
    assert!(format!("{:?}", notices.try_recv().unwrap()).contains(messages::CARGO_HOLD_FULL));
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(10));
    assert_eq!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap(), baseline);
    f.db.execute("UPDATE game.items SET quantity = 2 WHERE item_id = $1", &[&item]).await.unwrap();
    let result = extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, infinity_core::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap();
    assert!(!result.repeat && result.outcome == Some(crate::lua::events::ModuleCycleOutcome::Completed));
    assert!(format!("{:?}", notices.try_recv().unwrap()).contains(messages::CARGO_HOLD_FULL));
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(9));
    let logs = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap();
    assert_eq!(logs.len(), baseline.len() + 1);
    assert_eq!((logs[0].type_id, logs[0].solar_system_id, logs[0].quantity, logs[0].quantity_wasted), (ore_type, f.system, 1, 0));
    f.set(f.ship, ATTR_GENERAL_MINING_HOLD_CAPACITY, volume).await;
    let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.ok().unwrap();
    let (flag, capacity) = destination(&fit, ore_type);
    assert_eq!(flag, FLAG_GENERAL_MINING_HOLD, "positive mining-hold override destination: {:?}", (flag, capacity));
    let unit_volume = crate::services::item_volume::item_volume(&f.sde, ore_type, false, 1, None).ok().unwrap();
    let full_quantity = (capacity / unit_volume).ceil();
    assert!(full_quantity.is_finite() && full_quantity >= 1.0 && full_quantity < i32::MAX as f64);
    let mining_stack = crate::dao::items::transfers::insert(&f.db, ore_type, f.owner, f.ship, FLAG_GENERAL_MINING_HOLD, full_quantity as i64).await.unwrap();
    let result = extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, infinity_core::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap();
    assert!(!result.repeat && result.outcome == Some(crate::lua::events::ModuleCycleOutcome::CargoFull));
    let notice = format!("{:?}", notices.try_recv().unwrap());
    assert!(notice.contains(messages::MINING_HOLD_FULL), "unexpected notice: {notice}");
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(9));
    assert_eq!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap(), logs);
    f.set(f.ship, ATTR_GENERAL_MINING_HOLD_CAPACITY, volume * (i32::MAX as f64 + 2.0)).await;
    f.db.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&mining_stack, &i64::from(i32::MAX)]).await.unwrap();
    let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.ok().unwrap();
    let result = extract(&f.db, &f.world, &notify, f.owner, f.ship, f.system, &fit, module, module_type, infinity_core::consts::FLAG_HI_SLOT_FIRST, rock).await.ok().unwrap();
    assert!(result.repeat && result.outcome == Some(crate::lua::events::ModuleCycleOutcome::Completed));
    assert!(notices.try_recv().is_err());
    assert_eq!(crate::dao::asteroids::lock(&f.db, f.system, rock, ore_type).await.unwrap(), Some(7));
    let wide_logs = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await.unwrap();
    assert_eq!(wide_logs.len(), logs.len() + 1);
    assert_eq!((wide_logs[0].quantity, wide_logs[0].quantity_wasted), (2, 0));
    let stack = crate::services::items::load_items(&f.db, &f.sde, crate::dao::items::loading::ItemQuery::Id(mining_stack)).await.unwrap();
    assert_eq!(stack[0].quantity, i64::from(i32::MAX) + 2);
    }).await;
    let cleanup = crate::tests::catch_scenario(async {
        let published = if let Some(rock) = created_rock {
            let actor = f.world.asteroid_actor(f.system).expect("fixture asteroid actor");
            f.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &f.system, &f.system]).await.unwrap();
            Some(crate::space::asteroids::publish_depletion(&f.world, f.system, &actor, rock, f.system, None, None).await)
        } else { None };
        f.db.execute("DELETE FROM game.asteroid_fields WHERE field_id=$1 AND system_id=$1", &[&f.system]).await.unwrap();
        if let Some(published) = published { assert!(published); }
    }).await;
    f.cleanup().await;
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
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
            let rock = crate::dao::asteroids::insert_batch(&f.db, f.system, &[NewAsteroid { field: f.system, type_id, quantity: 5, radius: 1.0, pos: [0.0; 3] }]).await?[0].id;
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
    for rock in rocks { f.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &f.system, &f.system]).await?; }
    f.cleanup().await;
    result
}
