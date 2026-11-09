use super::*;
use crate::services::combat::active_modules;

#[test]
fn forced_inactive_is_excluded_from_fit_input_immediately() {
    let module = -9031;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        st.active.insert(module, ActiveModule { char_id: 1, stop: stop.clone(), forced_inactive: false });
    }
    assert!(active_modules().contains(&module));
    {
        let mut st = state();
        st.active.get_mut(&module).unwrap().forced_inactive = true;
        stop.store(true, Ordering::Relaxed);
    }
    assert!(!active_modules().contains(&module));
    assert!(state().active.remove(&module).is_some());
}

#[test]
fn ordinary_deactivation_remains_in_fit_input() {
    let module = -9032;
    let stop = Arc::new(AtomicBool::new(false));
    state().active.insert(module, ActiveModule { char_id: 1, stop: stop.clone(), forced_inactive: false });
    stop.store(true, Ordering::Relaxed);
    assert!(active_modules().contains(&module));
    state().active.remove(&module);
    assert!(!active_modules().contains(&module));
}

async fn fixture_fingerprint(db: &crate::db::Db) -> Vec<String> {
    let mut values = Vec::new();
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id, attribute_id), '')) AS fingerprint FROM game.item_attributes t",
    ] {
        values.push(db.query_one(sql, &[]).await.unwrap().get("fingerprint"));
    }
    values
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn forced_cycle_stop_publishes_once_and_consumes_propulsion_generation() {
    use infinity_core::consts::{dogma::ATTR_MASS, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_SPEED_FACTOR};
    use crate::space::{Ball, World, WarpState, MODE_STOP};
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let positive = |v: Option<f64>| v.is_some_and(|v| v.is_finite() && v > 0.0);
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && positive(t.fit_attribute(ATTR_MASS)) && positive(t.fit_attribute(ATTR_MAX_VELOCITY)))
        .expect("ship with positive mass and maxVelocity").type_id;
    let speed_boost = sde.dogma.attributes_by_name("speedBoostFactor").next().expect("speedBoostFactor attribute").attribute_id;
    let module_type = sde.types.iter().find(|t| t.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)
        && positive(t.fit_attribute(ATTR_SPEED_FACTOR)) && positive(t.fit_attribute(speed_boost)))
        .expect("microwarpdrive with positive speed factor and thrust").type_id;
    let scram_type = sde.types.iter().find(|t| sde.types.effects(t.type_id).iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_WARP_SCRAMBLE)).unwrap().type_id;
    let modifying = sde.dogma.effect(infinity_core::consts::dogma::EFFECT_WARP_SCRAMBLE).unwrap().modifiers().iter()
        .find(|row| row.domain == "targetID" && row.modified == ATTR_ACTIVATION_BLOCKED).unwrap().modifying;
    let before = fixture_fingerprint(&db).await;
    let (account, character, ship, module) = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('retire-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let character: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'retire-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
            let ship = crate::dao::items::creation::positioned(tx, ship_type, character, 1, [0.0; 3]).await?;
            let module = crate::dao::items::creation::rookie_ship(tx, module_type, character, ship, infinity_core::consts::FLAG_FITTING_FIRST).await?;
            Ok::<_, crate::db::Error>((account, character, ship, module))
        }),
        |fixture, ()| async move { Ok(fixture) },
    ).await.unwrap();
    let source = -ship;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_db.execute("UPDATE game.items SET online = true WHERE item_id = $1", &[&module]).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(character, tx.clone());
        let ball = Ball { id: ship, type_id: ship_type, owner_id: character, corp_id: 1, alliance_id: -1,
            char_id: Some(character), signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6,
            max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
            old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
            follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
            warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
            effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
        let mut source_ball = ball.clone();
        source_ball.id = source;
        source_ball.char_id = None;
        test_world.add_ship(1, ball).await.ok().expect("adding target ball");
        test_world.add_ship(1, source_ball).await.ok().expect("adding source ball");
        let baseline = Fit::load_in_space(&test_db, &test_world, 1, character, ship).await.ok().expect("loading baseline fit")
            .value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!(baseline.is_finite() && baseline > 0.0, "baseline maxVelocity: {baseline}");
        let tolerance = baseline.abs().max(1.0) * 1e-10;
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut st = state();
            st.active.insert(module, ActiveModule { char_id: character, stop: stop.clone(), forced_inactive: false });
            st.activation_generation.insert(module, stop.clone());
        }
        assert!(active_modules().contains(&module));
        let boosted = Fit::load_in_space(&test_db, &test_world, 1, character, ship).await.ok().expect("loading boosted fit")
            .value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!(boosted.is_finite() && boosted > baseline, "active maxVelocity {boosted} must exceed {baseline}");
        test_world.with(1, move |s| {
            s.upsert_projection(ship, super::super::super::projection::Projection {
                source_ship: source, source_module: -module, source_type: scram_type,
                effect_id: infinity_core::consts::dogma::EFFECT_WARP_SCRAMBLE,
                kind: EffectKind::Projected { jamming: super::super::super::JammingType::Scramble },
                until_ms: i64::MAX, values: vec![(modifying, 1.0)], charge_values: vec![], charge_type: None, scramble: None,
            });
            assert_eq!(s.projections_on(ship).len(), 1);
        }).await.unwrap();
        stop_blocked_propulsion(&test_db, &test_world, 1, ship).await;
        test_world.with(1, |_| ()).await.unwrap();
        assert!(stop.load(Ordering::Relaxed));
        assert!(!active_modules().contains(&module));
        {
            let st = state();
            assert!(st.active[&module].forced_inactive);
            assert!(Arc::ptr_eq(&st.active[&module].stop, &stop));
            assert!(Arc::ptr_eq(&st.activation_generation[&module], &stop));
        }
        let blocked = Fit::load_in_space(&test_db, &test_world, 1, character, ship).await.ok().expect("loading blocked fit")
            .value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!((blocked - baseline).abs() <= tolerance, "blocked maxVelocity {blocked} differs from baseline {baseline}");
        while let Ok((event, args)) = rx.try_recv() {
            let is_matching = event == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(module))
                && args.get(1) == Some(&Value::Int(i64::from(EFFECT_MICROWARPDRIVE)));
            assert!(!is_matching);
        }
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: character, ship_id: ship, system_id: 1 };
        let effect = registry::resolve(&sde, EFFECT_MICROWARPDRIVE).unwrap();
        let make_cycle = || Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: module_type, flag: infinity_core::consts::FLAG_FITTING_FIRST }],
            effect: effect.clone(), target: None, repeat: 3, stop: stop.clone(),
            owner: ActivationOwner { master: module, generation: stop.clone(), armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
            started: 123, charge_type: None, cycles: 1, duration_ms: 456.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        let first = make_cycle();
        let duplicate = make_cycle();
        test_world.with(1, move |s| { cycle_stop(s, first); cycle_stop(s, duplicate); }).await.unwrap();
        assert!(!state().active.contains_key(&module));
        let mut stops = Vec::new();
        while let Ok((event, args)) = rx.try_recv() {
            if event == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(module))
                && args.get(1) == Some(&Value::Int(i64::from(EFFECT_MICROWARPDRIVE)))
                && args.get(3) == Some(&Value::Int(0)) { stops.push(args); }
        }
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0][6], Value::Int(123));
        assert_eq!(stops[0][7], Value::Float(456.0));
        assert_eq!(stops[0][8], Value::Int(3));
        for _ in 0..50 {
            if !state().activation_generation.contains_key(&module) { break; }
            test_world.after(1, 100).await;
        }
        assert!(!state().activation_generation.contains_key(&module));
        let ordinary = Arc::new(AtomicBool::new(false));
        {
            let mut st = state();
            st.active.insert(module, ActiveModule { char_id: character, stop: ordinary.clone(), forced_inactive: false });
            st.activation_generation.insert(module, ordinary.clone());
        }
        ordinary.store(true, Ordering::Relaxed);
        assert!(active_modules().contains(&module));
        let deactivating = Fit::load_in_space(&test_db, &test_world, 1, character, ship).await.ok().expect("loading deactivating fit")
            .value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!((deactivating - boosted).abs() <= tolerance, "ordinary stop maxVelocity {deactivating} differs from boosted {boosted}");
        let cycle = Cycle { cx, modules: vec![Module { id: module, type_id: module_type, flag: infinity_core::consts::FLAG_FITTING_FIRST }],
            effect, target: None, repeat: 3, stop: ordinary.clone(),
            owner: ActivationOwner { master: module, generation: ordinary, armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
            started: 123, charge_type: None, cycles: 1, duration_ms: 456.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        test_world.with(1, move |s| cycle_stop(s, cycle)).await.unwrap();
        assert!(!active_modules().contains(&module));
        let retired = Fit::load_in_space(&test_db, &test_world, 1, character, ship).await.ok().expect("loading retired fit")
            .value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!((retired - baseline).abs() <= tolerance, "retired maxVelocity {retired} differs from baseline {baseline}");
        for _ in 0..50 {
            if !state().activation_generation.contains_key(&module) { break; }
            test_world.after(1, 100).await;
        }
        assert!(!state().activation_generation.contains_key(&module));
    }).await;
    world.remove_ball(1, source).await;
    world.remove_ball(1, ship).await;
    world.unregister_pilot_generation(character, None);
    let flush = world.flush_positions().await;
    {
        let mut st = state();
        st.active.remove(&module);
        st.activation_generation.remove(&module);
    }
    crate::services::combat::forget_ship(ship);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&&[module, ship][..]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2 AND location_id = $3", &[&module, &character, &ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2", &[&ship, &character]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&character, &account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    assert_eq!(fixture_fingerprint(&db).await, before);
    flush.unwrap();
    result.expect("forced retirement scenario failed");
}
