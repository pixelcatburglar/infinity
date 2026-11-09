//! Real-database regression for empty ancillary automatic reload reservations and actor completion.
use super::*;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use infinity_core::{
    consts::{CATEGORY_CHARGE, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_FUELED_SHIELD_BOOSTING, FLAG_FITTING_FIRST},
};
use crate::{
    db::{Db, Error},
    space::{Ball, World, WarpState, MODE_STOP},
};

const RELOAD_MS: f64 = 5_000.0;
const CARGO_QUANTITY: i64 = 1_i64 << 32;
const INCOMPATIBLE_QUANTITY: i64 = 7;

#[derive(Clone, Copy)]
struct Fixture { account: i64, character: i64, ship: i64, module: i64, cargo: i64, incompatible: i64 }

async fn create(db: &Db, ship_type: i32, module_type: i32, charge_type: i32, other_type: i32, system: i64) -> Result<Fixture, Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('reload-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'reload-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES ($1, $2, $3, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&ship_type, &character, &system]).await?.get("item_id");
        let module = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, online) VALUES ($1, $2, $3, $4, 1, true, true) RETURNING item_id", &[&module_type, &character, &ship, &FLAG_FITTING_FIRST]).await?.get("item_id");
        tx.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1, $2, $3)", &[&module, &ATTR_RELOAD_TIME, &RELOAD_MS]).await?;
        let cargo = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, false) RETURNING item_id", &[&charge_type, &character, &ship, &FLAG_CARGO, &CARGO_QUANTITY]).await?.get("item_id");
        let incompatible = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, false) RETURNING item_id", &[&other_type, &character, &ship, &FLAG_CARGO, &INCOMPATIBLE_QUANTITY]).await?.get("item_id");
        Ok(Fixture { account, character, ship, module, cargo, incompatible })
    }), |fixture, ()| async move { Ok(fixture) }).await
}

async fn cleanup(db: &Db, f: Fixture) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 FOR UPDATE", &[&f.ship]).await?;
        tx.execute("DELETE FROM game.pending_reloads WHERE ship_id = $1 AND module_id = $2 AND owner_id = $3", &[&f.ship, &f.module, &f.character]).await?;
        tx.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&f.ship, &FLAG_FITTING_FIRST]).await?;
        tx.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&f.module]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id = ANY($1) AND owner_id = $2 AND location_id = $3", &[&[f.cargo, f.incompatible, f.module].as_slice(), &f.character, &f.ship]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2", &[&f.ship, &f.character]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&f.character, &f.account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&f.account]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await
}

async fn fingerprint(db: &Db) -> Result<Vec<String>, Error> {
    let mut values = Vec::new();
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id, attribute_id), '')) AS fingerprint FROM game.item_attributes t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY ship_id, flag_id), '')) AS fingerprint FROM game.module_charges t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY reload_id), '')) AS fingerprint FROM game.pending_reloads t",
    ] { values.push(db.query_one(sql, &[]).await?.get("fingerprint")); }
    Ok(values)
}

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
    let before = fingerprint(&db).await.unwrap();
    let fixture = create(&db, ship_type, module_type, charge_type, other_type, system).await.unwrap();
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
        assert_eq!(fit.value(Key::Module(fixture.module), ATTR_RELOAD_TIME), RELOAD_MS);
        let condition = crate::services::combat::prepare_condition_with_fit(&test_db, fixture.ship, &fit).await.unwrap();
        cx.world.with(system, move |s| s.install_condition(fixture.ship, condition.into_condition(), fit)).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, FLAG_FITTING_FIRST).await.ok().expect("read empty magazine"), None);
        test_started.store(true, Ordering::Relaxed);
        auto_reload(&cx, fixture.module).await.ok().expect("automatic reload");
        let marker = *state().reloading.get(&fixture.module).expect("reload lockout marker");
        assert!(marker > crate::client::filetime_now());
        let rows = crate::dao::items::pending_reloads::for_flags(&test_db, fixture.ship, &[FLAG_FITTING_FIRST]).await.unwrap();
        assert_eq!(rows.len(), 1);
        let reserved = rows[0].quantity;
        assert!(reserved > 0 && reserved < CARGO_QUANTITY);
        assert_eq!(rows[0].module_id, fixture.module);
        assert_eq!(rows[0].type_id, charge_type);
        assert_eq!(quantity(&test_db, fixture.cargo).await, CARGO_QUANTITY - reserved);
        assert_eq!(quantity(&test_db, fixture.incompatible).await, INCOMPATIBLE_QUANTITY);
        assert_eq!(loaded(&test_db, fixture.ship, FLAG_FITTING_FIRST).await.ok().expect("read pending magazine"), None);
        auto_reload(&cx, fixture.module).await.ok().expect("duplicate automatic reload");
        let again = crate::dao::items::pending_reloads::for_flags(&test_db, fixture.ship, &[FLAG_FITTING_FIRST]).await.unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].reload_id, rows[0].reload_id);
        assert_eq!(quantity(&test_db, fixture.cargo).await, CARGO_QUANTITY - reserved);
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
    let final_cargo = quantity(&db, fixture.cargo).await;
    let final_other = quantity(&db, fixture.incompatible).await;
    let final_pending = crate::dao::items::pending_reloads::for_flags(&db, fixture.ship, &[FLAG_FITTING_FIRST]).await;
    world.remove_ball(system, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    let flush = world.flush_positions().await;
    state().reloading.remove(&fixture.module);
    super::super::super::forget_ship(fixture.ship);
    drop(world);
    cleanup(&db, fixture).await.unwrap();
    assert_eq!(fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    let (reserved, marker) = result.unwrap();
    let completion = completion.expect("actor reload completion notification");
    super::charge_sync::assert_prime(&completion, &sde, fixture.ship, FLAG_FITTING_FIRST, charge_type, reserved);
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
    let fixture = create(&db, ship_type, module_type, charge_type, charge_type, system).await.unwrap();
    let world = World::new(db.clone(), sde, 1);
    let (owner_tx, mut owner_rx) = tokio::sync::mpsc::unbounded_channel();
    let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
    let target = -fixture.ship;
    let target_pilot = -fixture.character;
    world.register_pilot(fixture.character, owner_tx.clone());
    world.register_pilot(target_pilot, target_tx);
    world.add_ship(system, ball(fixture.ship, ship_type, fixture.character)).await.unwrap();
    world.add_ship(system, ball(target, ship_type, target_pilot)).await.unwrap();
    let cx = Ctx { db: db.clone(), world: world.clone(), notify: owner_tx, char_id: fixture.character, ship_id: fixture.ship, system_id: system };
    let test_cx = cx.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let cx = test_cx;
        let world = test_world;
        for case in 0..5 {
            let generation = Arc::new(AtomicBool::new(false));
            state().active.insert(fixture.module, ActiveModule {
                char_id: fixture.character, stop: generation.clone(), forced_inactive: false,
            });
            add_target_for_cycle(&cx, target, false, fixture.module, generation.clone()).await.ok().expect("cycle lock starts");
            {
                let mut st = state();
                match case {
                    0 => generation.store(true, Ordering::Relaxed),
                    1 => st.active.get_mut(&fixture.module).unwrap().stop = Arc::new(AtomicBool::new(false)),
                    2 => {
                        st.active.get_mut(&fixture.module).unwrap().forced_inactive = true;
                        generation.store(true, Ordering::Relaxed);
                    }
                    3 => { st.active.remove(&fixture.module); }
                    _ => {}
                }
            }
            let (_, args) = tokio::time::timeout(Duration::from_secs(120), owner_rx.recv()).await.unwrap().unwrap();
            if case == 0 || case == 2 {
                assert_eq!(args[0].as_bytes(), Some(b"lost".as_slice()));
                assert!(!state().locking.contains_key(&(fixture.character, target)));
                assert!(!targets_of(fixture.character).contains(&target));
                assert!(target_rx.try_recv().is_err());
            } else {
                assert_eq!(args[0].as_bytes(), Some(b"add".as_slice()));
                let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
                assert_eq!(args[0].as_bytes(), Some(b"otheradd".as_slice()));
                clear_character_locks(&world, fixture.character);
                let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
                assert_eq!(args[0].as_bytes(), Some(b"otherlost".as_slice()));
            }
            state().active.remove(&fixture.module);
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
    state().active.remove(&fixture.module);
    world.remove_ball(system, fixture.ship).await;
    world.remove_ball(system, target).await;
    world.unregister_pilot_generation(fixture.character, None);
    world.unregister_pilot_generation(target_pilot, None);
    drop(cx);
    drop(world);
    cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
