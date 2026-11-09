use super::*;
use infinity_core::{consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_RECHARGE_RATE, CATEGORY_SHIP}};
use crate::{dao::items::{combat, pending_reloads, creation}, services::combat::groups_health::{capacitor_snapshot, drain_capacitor}, space::{Ball, World, WarpState, MODE_STOP}};
use crate::services::effects::weapons::consume_charge;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn refused_charge_consumption_refunds_debit() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_RECHARGE_RATE).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('refund-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'refund-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let ship = creation::positioned(&db, ship_type, character, 1, [0.0; 3]).await.unwrap();
    let module = Module { id: ship, type_id: ship_type, flag: 1 };
    combat::apply_loads(&db, ship, &[(module.id, module.flag, ship_type, 1, 0)]).await.unwrap();
    let reload = pending_reloads::insert(&db, character, ship, module.id, module.flag, ship_type, 1, infinity_core::consts::FLAG_CARGO, ship).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        w.register_pilot(character, tx.clone());
        let ball = Ball { id: ship, type_id: ship_type, owner_id: character, corp_id: 1, alliance_id: -1,
            char_id: Some(character), signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6,
            max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
            old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
            follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
            warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
            effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
        w.add_ship(1, ball).await.unwrap();
        let cx = Ctx { db: d.clone(), world: w.clone(), notify: tx, char_id: character, ship_id: ship, system_id: 1 };
        let fit = Fit::load(&d, sde, character, ship).await.ok().expect("load fit");
        let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let debit = capacity / 2.0;
        assert!(drain_capacitor(&cx, &fit, debit));
        assert_eq!(consume_charge(&cx, &module, Some(ship_type)).await.ok().expect("consume during reload"), None);
        refund_refused_charge(&cx, debit).await.ok().expect("refund capacitor");
        let level = capacitor_snapshot(&d, &w, 1, ship).await.ok().expect("snapshot capacitor").unwrap().level;
        assert!((level - capacity).abs() < 1e-6);
        assert_eq!(loaded(&d, ship, module.flag).await.ok().expect("read loaded charge"), Some((ship_type, 1)));
        pending_reloads::take(&d, reload).await.unwrap();
        assert_eq!(consume_charge(&cx, &module, Some(ship_type + 1)).await.ok().expect("consume mismatched charge"), None);
        assert_eq!(loaded(&d, ship, module.flag).await.ok().expect("read loaded charge"), Some((ship_type, 1)));
    }).await;
    world.remove_ball(1, ship).await;
    world.unregister_pilot_generation(character, None);
    world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(ship);
    pending_reloads::take(&db, reload).await.unwrap();
    combat::delete_charge(&db, ship, 1).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn positive_charge_consumption_only_publishes_quantity_delta() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let (module_type, charge_type) = crate::services::combat::targeting_charges::tests::compatible_ammunition(&sde);
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("published ship").type_id;
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let fixture = crate::services::effects::tests::fixture::create(&db, ship_type, module_type, flag).await.unwrap();
    let world = World::new(db.clone(), sde, 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let system = 1;
    let result = tokio::spawn(async move {
        combat::apply_loads(&test_db, fixture.ship, &[(fixture.module, flag, charge_type, 2, 0)]).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(fixture.character, tx.clone());
        let ball = crate::services::effects::tests::fixture::ball(fixture.ship, ship_type, fixture.character, Some(fixture.character), 0.0);
        crate::services::effects::tests::fixture::admit(&test_db, &test_world, system, ball).await;
        while rx.try_recv().is_ok() {}
        let ship = fixture.ship;
        let revision = test_world.with(system, move |s| s.combat_fit_revision(ship)).await.unwrap().expect("installed actor fit");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: fixture.character, ship_id: ship, system_id: system };
        let module = Module { id: fixture.module, type_id: module_type, flag };
        assert_eq!(consume_charge(&cx, &module, Some(charge_type)).await.unwrap(), Some(charge_type));
        assert_eq!(loaded(&test_db, ship, flag).await.unwrap(), Some((charge_type, 1)));
        let (event, args) = rx.try_recv().expect("quantity delta");
        assert_eq!(event, "OnModuleAttributeChanges");
        let rows = args[0].items().unwrap();
        assert_eq!(rows.len(), 1);
        let fields = rows[0].items().unwrap();
        assert_eq!(fields[2], crate::services::combat::targeting_charges::charge_key(ship, flag, charge_type));
        assert_eq!(fields[3].as_int(), Some(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY)));
        assert_eq!(fields[5].as_int(), Some(1));
        assert_eq!(fields[6].as_int(), Some(2));
        assert_eq!(test_world.with(system, move |s| s.combat_fit_revision(ship)).await.unwrap(), Some(revision));
        assert!(rx.try_recv().is_err(), "consumption must not prime or publish fitting");
    }).await;
    world.remove_ball(system, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(fixture.ship);
    crate::services::effects::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
