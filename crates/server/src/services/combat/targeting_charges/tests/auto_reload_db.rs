//! Real-database regression for empty ancillary automatic reload reservations and actor completion.
use super::*;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use infinity_core::{
    consts::{CATEGORY_CHARGE, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_FUELED_SHIELD_BOOSTING, FLAG_FITTING_FIRST},
};
use crate::{
    db::Db,
    services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation},
    space::{Ball, World, WarpState, MODE_STOP},
};

const RELOAD_MS: f64 = 5_000.0;
const CARGO_QUANTITY: i64 = 1_i64 << 32;
const INCOMPATIBLE_QUANTITY: i64 = 7;

fn ball(id: i64, type_id: i32, pilot: i64) -> Ball {
    Ball {
        id, type_id, owner_id: pilot, corp_id: 1, alliance_id: -1,
        char_id: Some(pilot), signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

async fn quantity(db: &Db, item: i64) -> i64 {
    db.query_opt("SELECT quantity FROM game.items WHERE item_id = $1", &[&item]).await.unwrap()
        .map(|row| row.get("quantity")).unwrap_or(0)
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn ancillary_auto_reload_db_smoke() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let size_id = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let (module_type, charge_type, other_type) = sde.types.iter().filter(|m| m.published == Some(true)
        && m.category_id == Some(CATEGORY_MODULE) && m.effects().iter().any(|e| e.effect_id == EFFECT_FUELED_SHIELD_BOOSTING))
        .find_map(|m| {
            let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
                .filter_map(|a| m.attribute(a.attribute_id)).collect();
            let size = size_id.and_then(|id| m.attribute(id));
            let matches = |c: &crate::sde::types::TypeData| charge_matches(groups.iter().copied(), c.group_id, size, size_id.and_then(|id| c.attribute(id)));
            let capacity = m.capacity.unwrap_or(0.0).max(m.attribute(ATTR_CAPACITY).unwrap_or(0.0));
            let charge = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && matches(c) && c.volume.is_some_and(|v| v > 0.0 && capacity >= v && capacity / v < CARGO_QUANTITY as f64) )?;
            let other = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE) && !matches(c))?;
            Some((m.type_id, charge.type_id, other.type_id))
        }).expect("published fueled shield module and compatible and incompatible charges");
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("published ship").type_id;
    let system = 1;
    let fixture = create_ship(&db, ship_type, ShipLocation::Docked { station: system }, &[
        ShipItemSpec { type_id: module_type, flag_id: FLAG_FITTING_FIRST, quantity: 1, singleton: true, online: true, attributes: &[(ATTR_RELOAD_TIME, RELOAD_MS)] },
        ShipItemSpec { type_id: charge_type, flag_id: FLAG_CARGO, quantity: CARGO_QUANTITY, singleton: false, online: false, attributes: &[] },
        ShipItemSpec { type_id: other_type, flag_id: FLAG_CARGO, quantity: INCOMPATIBLE_QUANTITY, singleton: false, online: false, attributes: &[] },
    ]).await.unwrap();
    let (module, cargo, incompatible) = (fixture.module(), fixture.items[1], fixture.items[2]);
    let world = World::new(db.clone(), sde.clone(), 1);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let started = Arc::new(AtomicBool::new(false));
    let test_started = started.clone();
    let test_db = db.clone();
    let test_world = world.clone();
    let test_sde = sde.clone();
    let result = tokio::spawn(async move {
        test_world.register_pilot(fixture.character, tx.clone());
        test_world.add_ship(system, ball(fixture.ship, ship_type, fixture.character)).await.unwrap();
        let cx = Ctx { db: test_db.clone(), world: test_world, notify: tx, char_id: fixture.character, ship_id: fixture.ship, system_id: system };
        let fit = Fit::load(&test_db, test_sde, fixture.character, fixture.ship).await.ok().expect("load fit");
        assert_eq!(fit.value(Key::Module(module), ATTR_RELOAD_TIME), RELOAD_MS);
        let condition = crate::services::combat::prepare_condition_with_fit(&test_db, fixture.ship, &fit).await.unwrap();
        cx.world.with(system, move |s| s.install_condition(fixture.ship, condition.into_condition(), fit)).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, FLAG_FITTING_FIRST).await.ok().expect("read empty magazine"), None);
        test_started.store(true, Ordering::Relaxed);
        auto_reload(&cx, module).await.ok().expect("automatic reload");
        let marker = *state().reloading.get(&module).expect("reload lockout marker");
        assert!(marker > crate::client::filetime_now());
        let rows = crate::dao::items::pending_reloads::for_flags(&test_db, fixture.ship, &[FLAG_FITTING_FIRST]).await.unwrap();
        assert_eq!(rows.len(), 1);
        let reserved = rows[0].quantity;
        assert!(reserved > 0 && reserved < CARGO_QUANTITY);
        assert_eq!(rows[0].module_id, module);
        assert_eq!(rows[0].type_id, charge_type);
        assert_eq!(quantity(&test_db, cargo).await, CARGO_QUANTITY - reserved);
        assert_eq!(quantity(&test_db, incompatible).await, INCOMPATIBLE_QUANTITY);
        assert_eq!(loaded(&test_db, fixture.ship, FLAG_FITTING_FIRST).await.ok().expect("read pending magazine"), None);
        auto_reload(&cx, module).await.ok().expect("duplicate automatic reload");
        let again = crate::dao::items::pending_reloads::for_flags(&test_db, fixture.ship, &[FLAG_FITTING_FIRST]).await.unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].reload_id, rows[0].reload_id);
        let blocked = cx.world.with(system, move |s| (
            s.hold_loaded_use(fixture.ship, module).is_none(),
            s.loaded_empty_proof(fixture.ship, module),
        )).await.unwrap();
        assert_eq!(blocked, (true, false), "a pending reservation keeps an empty slot that refuses charged use and empty proof");
        assert_eq!(quantity(&test_db, cargo).await, CARGO_QUANTITY - reserved);
        (reserved, marker)
    }).await;
    // Keep the receiver outside the task so an assertion panic still drains the actual timer.
    let completion = if started.load(Ordering::Relaxed) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some((name, args)) = rx.recv().await {
                if name == "OnGodmaPrimeItem" {
                    return Some(args);
                }
            }
            None
        }).await.unwrap_or(None)
    } else { None };
    let observed_at = crate::client::filetime_now();
    let final_loaded = loaded(&db, fixture.ship, FLAG_FITTING_FIRST).await.ok();
    let final_cargo = quantity(&db, cargo).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while state().reloading.contains_key(&module) { tokio::task::yield_now().await; }
    }).await.expect("reload completion cleared its marker");
    let effective = world.with(system, move |s| s.loaded_effective(fixture.ship, module)).await.flatten();
    assert_eq!(effective.map(|view| (view.1, view.2, view.3)), final_loaded.flatten().map(|(_, quantity)| (charge_type, quantity, false)),
        "the completed magazine publishes its exact quantity with pending cleared");
    let final_other = quantity(&db, incompatible).await;
    let final_pending = crate::dao::items::pending_reloads::for_flags(&db, fixture.ship, &[FLAG_FITTING_FIRST]).await;
    world.remove_ball(system, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    let flush = world.flush_positions().await;
    state().reloading.remove(&module);
    super::super::super::forget_ship(fixture.ship);
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: fixture.account, character: fixture.character }], &[fixture.ship, module, cargo, incompatible]).await.unwrap();
    cleanup_ship(&db, None, fixture).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
    let (reserved, marker) = result.unwrap();
    let completion = completion.expect("actor reload completion notification");
    let Value::Instance(_, info) = &completion[1] else { panic!("prime info") };
    assert!(info.get("time").and_then(Value::as_int).unwrap() >= marker);
    assert!(observed_at >= marker);
    assert_eq!(final_loaded.expect("read loaded magazine"), Some((charge_type, reserved)));
    assert_eq!(final_cargo + reserved, CARGO_QUANTITY);
    assert_eq!(final_other, INCOMPATIBLE_QUANTITY);
    assert!(final_pending.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn actor_completes_passive_and_normal_locks_with_symmetric_notifications() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_MAX_TARGET_RANGE).unwrap_or(0.0) > 0.0
        && t.attribute(ATTR_SIGNATURE_RADIUS).unwrap_or(0.0) > 0.0)
        .max_by(|a, b| a.attribute(ATTR_SCAN_RESOLUTION).unwrap_or(0.0).total_cmp(&b.attribute(ATTR_SCAN_RESOLUTION).unwrap_or(0.0)))
        .expect("ship type").type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)).expect("module type").type_id;
    let charge_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_CHARGE)).expect("charge type").type_id;
    let system = 1;
    let fixture = create_ship(&db, ship_type, ShipLocation::Docked { station: system }, &[
        ShipItemSpec { type_id: module_type, flag_id: FLAG_FITTING_FIRST, quantity: 1, singleton: true, online: true, attributes: &[(ATTR_RELOAD_TIME, RELOAD_MS)] },
        ShipItemSpec { type_id: charge_type, flag_id: FLAG_CARGO, quantity: CARGO_QUANTITY, singleton: false, online: false, attributes: &[] },
        ShipItemSpec { type_id: charge_type, flag_id: FLAG_CARGO, quantity: INCOMPATIBLE_QUANTITY, singleton: false, online: false, attributes: &[] },
    ]).await.unwrap();
    let module = fixture.module();
    let world = World::new(db.clone(), sde, 1);
    let (owner_tx, mut owner_rx) = tokio::sync::mpsc::unbounded_channel();
    let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
    let target = -fixture.ship;
    let target_pilot = -fixture.character;
    world.register_pilot(fixture.character, owner_tx.clone());
    world.register_pilot(target_pilot, target_tx);
    world.add_ship(system, ball(fixture.ship, ship_type, fixture.character)).await.unwrap();
    world.add_ship(system, ball(target, ship_type, target_pilot)).await.unwrap();
    let source_fit = Fit::load_in_space(&db, &world, system, fixture.character, fixture.ship).await.unwrap();
    let source_condition = crate::services::combat::prepare_condition_with_fit(&db, fixture.ship, &source_fit).await.unwrap();
    world.with(system, move |s| s.install_condition(fixture.ship, source_condition.into_condition(), source_fit)).await.unwrap();
    let cx = Ctx { db: db.clone(), world: world.clone(), notify: owner_tx, char_id: fixture.character, ship_id: fixture.ship, system_id: system };
    let test_cx = cx.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let cx = test_cx;
        let world = test_world;
        for case in 0..5 {
            let generation = Arc::new(AtomicBool::new(false));
            state().active.insert(module, ActiveModule {
                char_id: fixture.character, stop: generation.clone(), forced_inactive: false,
            });
            add_target_for_cycle(&cx, target, case == 0, module, generation.clone()).await.ok().expect("cycle lock starts");
            let attempt;
            {
                let mut st = state();
                attempt = st.locking[&(fixture.character, target)].attempt;
                match case {
                    0 => generation.store(true, Ordering::Relaxed),
                    1 => st.active.get_mut(&module).unwrap().stop = Arc::new(AtomicBool::new(false)),
                    2 => {
                        st.active.get_mut(&module).unwrap().forced_inactive = true;
                        generation.store(true, Ordering::Relaxed);
                    }
                    3 => { st.active.remove(&module); }
                    _ => {}
                }
            }
            if case == 0 || case == 2 {
                tokio::time::timeout(Duration::from_secs(120), async {
                    while state().locking.contains_key(&(fixture.character, target)) {
                        assert!(world.after(system, 100).await);
                    }
                }).await.unwrap();
                if case == 0 {
                    assert_eq!(owner_rx.try_recv().unwrap(), crate::client::target_lock_failure(fixture.character, target, attempt));
                } else { assert!(owner_rx.try_recv().is_err()); }
                assert!(!targets_of(fixture.character).contains(&target));
                assert!(target_rx.try_recv().is_err());
            } else {
                let (_, args) = tokio::time::timeout(Duration::from_secs(120), owner_rx.recv()).await.unwrap().unwrap();
                assert_eq!(args[0].as_bytes(), Some(b"add".as_slice()));
                let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
                assert_eq!(args[0].as_bytes(), Some(b"otheradd".as_slice()));
                clear_character_locks(&world, fixture.character);
                let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
                assert_eq!(args[0].as_bytes(), Some(b"otherlost".as_slice()));
            }
            state().active.remove(&module);
        }
        add_target_ctx(&cx, target, true).await.ok().expect("passive lock starts");
        let (_, args) = tokio::time::timeout(Duration::from_secs(120), owner_rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"add".as_slice()));
        assert_eq!(args[1].as_int(), Some(target));
        assert!(targets_of(fixture.character).contains(&target));
        assert!(target_rx.try_recv().is_err());
        clear_character_locks(&world, fixture.character);
        assert!(target_rx.try_recv().is_err());
        add_target_ctx(&cx, target, false).await.ok().expect("normal lock starts");
        let (_, args) = tokio::time::timeout(Duration::from_secs(120), owner_rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"add".as_slice()));
        let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"otheradd".as_slice()));
        assert_eq!(args[1].as_int(), Some(fixture.ship));
        clear_character_locks(&world, fixture.character);
        let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"otherlost".as_slice()));
        assert_eq!(args[1].as_int(), Some(fixture.ship));
    }).await;
    clear_character_locks(&world, fixture.character);
    state().active.remove(&module);
    world.remove_ball(system, fixture.ship).await;
    world.remove_ball(system, target).await;
    world.unregister_pilot_generation(fixture.character, None);
    world.unregister_pilot_generation(target_pilot, None);
    drop(cx);
    drop(world);
    cleanup_ship(&db, None, fixture).await.unwrap();
    result.unwrap();
}
