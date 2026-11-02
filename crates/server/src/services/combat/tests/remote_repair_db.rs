//! Database-backed remote repair activation and guarded mutation regressions.
use super::*;
use crate::{
    config::Config, conn::Conn, consts::{CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER, dogma::EFFECT_REMOTE_ARMOR_REPAIRER, dogma::EFFECT_REMOTE_HULL_REPAIRER, dogma::EFFECT_REMOTE_SHIELD_BOOSTER, FLAG_FITTING_FIRST},
    db::Error, handshake::Login,
    services::effects::registry::{self, EffectAttribute},
    space::{WarpState, MODE_STOP},
};

mod ancillary;

#[derive(Clone, Copy)]
pub(super) struct Fixture { pub(super) account: i64, pub(super) source: i64, pub(super) target: i64, pub(super) ship: i64, pub(super) victim: i64, pub(super) module: i64 }

pub(super) async fn create(db: &Db, ship_type: i32, module_type: i32, system: i64) -> Result<Fixture, Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('remote-repair-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let source = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'remote-repair-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let target = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'remote-repair-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES ($1, $2, $3, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&ship_type, &source, &system]).await?.get("item_id");
        let victim = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES ($1, $2, $3, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&ship_type, &target, &system]).await?.get("item_id");
        let module = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, online) VALUES ($1, $2, $3, $4, 1, true, true) RETURNING item_id", &[&module_type, &source, &ship, &FLAG_FITTING_FIRST]).await?.get("item_id");
        Ok(Fixture { account, source, target, ship, victim, module })
    }), |f, ()| async move { Ok(f) }).await
}

pub(super) async fn cleanup(db: &Db, f: Fixture) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2 AND location_id = $3", &[&f.module, &f.source, &f.ship]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&&[f.ship, f.victim][..]]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = ANY($1) AND user_id = $2", &[&&[f.source, f.target][..], &f.account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&f.account]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await
}

pub(super) async fn fingerprint(db: &Db) -> Result<Vec<String>, Error> {
    let mut result = Vec::new();
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id, attribute_id), '')) AS fingerprint FROM game.item_attributes t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY ship_id, flag_id), '')) AS fingerprint FROM game.module_charges t",
    ] { result.push(db.query_one(sql, &[]).await?.get("fingerprint")); }
    Ok(result)
}

pub(super) fn ball(id: i64, type_id: i32, pilot: i64) -> Ball {
    Ball { id, type_id, owner_id: pilot, corp_id: 1, alliance_id: -1, char_id: Some(pilot), signature_radius: None,
        mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

async fn run_remote_repair(effect_id: i32, layer: usize, interrupt: bool) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, effect_id).expect("remote repair effect");
    let effect_name = sde.dogma.effect(effect_id).unwrap().name.clone().unwrap();
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == effect_id)
        && (effect_id == EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER || (effect.attribute(EffectAttribute::Duration).and_then(|a| t.attribute(a)).is_some_and(|v| v >= 3000.0 && v <= 30000.0)
        && effect.attribute(EffectAttribute::Discharge).and_then(|a| t.attribute(a)).is_some_and(|v| v > 0.0)
        && t.attribute([crate::consts::dogma::ATTR_SHIELD_BONUS, crate::consts::dogma::ATTR_ARMOR_DAMAGE_AMOUNT, crate::consts::dogma::ATTR_STRUCTURE_DAMAGE_AMOUNT][layer]).is_some_and(|v| v > 0.0)))).expect("remote repair module").type_id;
    let ship_type = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE, ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE].iter().all(|&a| t.attribute(a).is_some_and(|v| v > 10.0)))
        .max_by(|a, b| a.attribute(ATTR_CAPACITOR_CAPACITY).unwrap().total_cmp(&b.attribute(ATTR_CAPACITOR_CAPACITY).unwrap())).expect("ship with health and capacitor").type_id;
    let system = 1;
    let before = fingerprint(&db).await.unwrap();
    let f = create(&db, ship_type, module_type, system).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.source, session_id: f.source, language_id: "EN".into() };
        let mut client = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), test_world.clone()), test_world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(f.source)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(system))]);
        let (source_tx, mut source_rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = source_tx;
        let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.target, target_tx);
        test_world.add_ship(system, ball(f.ship, ship_type, f.source)).await.unwrap();
        test_world.add_ship(system, ball(f.victim, ship_type, f.target)).await.unwrap();
        let (destiny_tx, mut destiny_rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.observe(system, f.source, f.ship, destiny_tx).await.unwrap();
        let source_stats = ship_stats(&test_db, &test_world, system, f.source, f.ship).await.ok().unwrap();
        let target_stats = ship_stats(&test_db, &test_world, system, f.target, f.victim).await.ok().unwrap();
        for (ship, stats) in [(f.ship, source_stats), (f.victim, target_stats)] {
            state().condition.insert(ship, Condition { now: stats.max.map(|v| v * 0.5), stats, shield_at: filetime_now() + 60 * FILETIME_SECOND });
        }
        let source_before = state().condition[&f.ship].now;
        let target_before = state().condition[&f.victim].now;
        let args = [Value::Int(f.module), Value::str(effect_name), Value::Int(f.victim), Value::Int(1)];
        let kwargs = Value::Dict(vec![]);
        let call = Call { service: "dogmaIM", method: "Activate", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        if effect_id == EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER {
            let refused = crate::services::combat::dispatch(&mut client, &call).await.expect("combat method");
            assert!(matches!(refused, Err(CallError::User { msg, .. }) if msg == crate::messages::MSG_CUSTOM_NOTIFY));
            assert!(crate::services::effects::unsupported::snapshot().contains(&(args[1].as_text().unwrap().into_owned(), module_type)));
            assert_eq!(state().condition[&f.victim].now, target_before);
            return;
        }
        assert!(crate::services::combat::dispatch(&mut client, &call).await.expect("combat method").is_err(), "unlocked target must be refused");
        state().targets.entry(f.source).or_default().insert(f.victim);
        assert!(crate::services::combat::dispatch(&mut client, &call).await.expect("combat method").is_ok());
        if layer != 0 { assert_eq!(state().condition[&f.victim].now[layer], target_before[layer]); }
        let (event, payload) = source_rx.try_recv().unwrap();
        assert_eq!(event, "OnGodmaShipEffect");
        assert_eq!(&payload[..2], &[Value::Int(f.module), Value::Int(i64::from(effect_id))]);
        assert_eq!(payload[3], Value::Int(1));
        let fit = Fit::load_in_space(&test_db, &test_world, system, f.source, f.ship).await.ok().unwrap();
        let amount = fit.value(Key::Module(f.module), [crate::consts::dogma::ATTR_SHIELD_BONUS, crate::consts::dogma::ATTR_ARMOR_DAMAGE_AMOUNT, crate::consts::dogma::ATTR_STRUCTURE_DAMAGE_AMOUNT][layer]);
        let expected = (target_before[layer] + amount).min(state().condition[&f.victim].stats.max[layer]);
        let duration = fit.value(Key::Module(f.module), effect.attribute(EffectAttribute::Duration).unwrap()).max(250.0) as u64;
        assert!(test_world.after(system, 1500).await);
        if layer == 0 {
            assert!(state().condition[&f.victim].now[layer] > target_before[layer], "shield repairs at cycle start");
        } else {
            assert_eq!(state().condition[&f.victim].now[layer], target_before[layer], "armor and hull repair at cycle end");
        }
        if interrupt {
            state().targets.remove(&f.source);
            assert!(test_world.after(system, duration + 1500).await);
            assert_eq!(state().condition[&f.victim].now[layer], target_before[layer], "interrupted armor cycle must not repair");
            let mut stops = Vec::new();
            while let Ok((event, payload)) = source_rx.try_recv() {
                if event == "OnGodmaShipEffect" && payload.first() == Some(&Value::Int(f.module))
                    && payload.get(1) == Some(&Value::Int(i64::from(effect_id))) && payload.get(3) == Some(&Value::Int(0)) {
                    stops.push(payload);
                }
            }
            assert_eq!(stops.len(), 1, "canonical effect stop");
            let mut phases = Vec::new();
            while let Ok(events) = destiny_rx.try_recv() {
                for event in events {
                    let Value::Tuple(parts) = event else { continue };
                    let Some(Value::Tuple(call)) = parts.get(1) else { continue };
                    if call.first() != Some(&Value::str("OnSpecialFX")) { continue; }
                    let Some(Value::Tuple(args)) = call.get(1) else { continue };
                    if args.first() == Some(&Value::Int(f.ship)) && args.get(1) == Some(&Value::Int(f.module))
                        && args.get(3) == Some(&Value::Int(f.victim)) {
                        phases.push(args[7].clone());
                    }
                }
            }
            assert_eq!(phases, vec![Value::Int(1), Value::Int(0)], "canonical FX start and stop");
            return;
        }
        assert!(test_world.after(system, duration + 1500).await);
        assert!(state().condition[&f.victim].now[layer] > target_before[layer]);
        assert!((state().condition[&f.victim].now[layer] - expected).abs() < 1e-7 * expected.max(1.0));
        let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let need = fit.value(Key::Module(f.module), effect.attribute(EffectAttribute::Discharge).unwrap());
        let remaining = state().capacitor[&f.ship].0;
        assert!((remaining - (capacity - need)).abs() < 1e-7 * capacity, "normal remote repair consumes capacitor");
        assert_eq!(state().condition[&f.ship].now, source_before);
        let (event, payload) = target_rx.try_recv().unwrap();
        assert_eq!(event, "OnModuleAttributeChanges");
        let [Value::List(changes)] = payload.as_slice() else { panic!("expected target HUD changes") };
        assert_eq!(changes.len(), 3);
        for (change, attr) in changes.iter().zip([ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE]) {
            let Value::Tuple(row) = change else { panic!("expected target attribute row") };
            assert_eq!(&row[..4], &[Value::str("OnModuleAttributeChange"), Value::Int(f.target), Value::Int(f.victim), Value::Int(i64::from(attr))]);
        }
        let repaired = state().condition[&f.victim].now[layer];
        state().targets.remove(&f.source);
        groups_health::repair_player(&test_db, &test_world, system, f.victim, f.target, layer, 1.0,
            move |_| state().targets.get(&f.source).is_some_and(|targets| targets.contains(&f.victim))).await.ok().unwrap();
        assert_eq!(state().condition[&f.victim].now[layer], repaired, "lost lock");
        let old_generation = Arc::new(AtomicBool::new(false));
        state().active.insert(f.module, ActiveModule { char_id: f.source, stop: Arc::new(AtomicBool::new(false)), forced_inactive: false });
        groups_health::repair_player(&test_db, &test_world, system, f.victim, f.target, layer, 1.0,
            move |_| state().active.get(&f.module).is_some_and(|active| Arc::ptr_eq(&active.stop, &old_generation))).await.ok().unwrap();
        assert_eq!(state().condition[&f.victim].now[layer], repaired, "stale generation");
        state().active.remove(&f.module);
        test_world.remove_ball(system, f.victim).await;
        let mut distant = ball(f.victim, ship_type, f.target);
        distant.pos = [1_000_000_000.0, 0.0, 0.0];
        distant.old_pos = distant.pos;
        test_world.add_ship(system, distant).await.unwrap();
        groups_health::repair_player(&test_db, &test_world, system, f.victim, f.target, layer, 1.0, move |s| {
            s.sampled_ball(f.ship).zip(s.sampled_ball(f.victim)).is_some_and(|(a, b)| crate::space::dist(a.pos, b.pos) < 1000.0)
        }).await.ok().unwrap();
        assert_eq!(state().condition[&f.victim].now[layer], repaired, "out of range");
        test_world.remove_ball(system, f.victim).await;
        groups_health::repair_player(&test_db, &test_world, system, f.victim, f.target, layer, 1.0, move |s| s.contains(f.victim)).await.ok().unwrap();
        assert_eq!(state().condition[&f.victim].now[layer], repaired, "target removed");
    }).await;
    world.unobserve(f.source);
    world.remove_ball(system, f.ship).await;
    world.remove_ball(system, f.victim).await;
    world.unregister_pilot(f.target);
    let flush = world.flush_positions().await;
    forget_ship(f.ship);
    forget_ship(f.victim);
    state().targets.remove(&f.source);
    drop(world);
    cleanup(&db, f).await.unwrap();
    assert_eq!(fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn remote_shield_activation_db_smoke() { run_remote_repair(EFFECT_REMOTE_SHIELD_BOOSTER, 0, false).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn remote_armor_activation_db_smoke() { run_remote_repair(EFFECT_REMOTE_ARMOR_REPAIRER, 1, false).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn remote_hull_activation_db_smoke() { run_remote_repair(EFFECT_REMOTE_HULL_REPAIRER, 2, false).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn mutadaptive_remote_refusal_db_smoke() { run_remote_repair(EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER, 1, false).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn remote_armor_lock_loss_interruption_db_smoke() { run_remote_repair(EFFECT_REMOTE_ARMOR_REPAIRER, 1, true).await; }
