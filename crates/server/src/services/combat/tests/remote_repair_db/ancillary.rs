//! Database-backed ancillary remote repair cycles through the dogma activation path.
//! Uses published SDE modules and charges, game items and module charges; no UI is involved.
use super::*;
use crate::consts::{dogma::ATTR_CAPACITOR_NEED, dogma::ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER, dogma::ATTR_RELOAD_TIME, CATEGORY_CHARGE, dogma::EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER, dogma::EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER, FLAG_CARGO};

const RELOAD_MS: f64 = 1_000.0;
const CARGO_QUANTITY: i32 = 100_000;
const OTHER_QUANTITY: i32 = 7;

async fn ancillary(effect_id: i32, layer: usize, charged: bool, reload: bool) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, effect_id).expect("ancillary remote effect");
    let name = sde.dogma.effect(effect_id).unwrap().name.clone().unwrap();
    let size_id = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let amount_attr = [crate::consts::dogma::ATTR_SHIELD_BONUS, crate::consts::dogma::ATTR_ARMOR_DAMAGE_AMOUNT][layer];
    let (module_type, charge_type, other_type) = sde.types.iter().filter(|m| m.published == Some(true)
        && m.category_id == Some(CATEGORY_MODULE) && m.effects().iter().any(|e| e.effect_id == effect_id)
        && effect.attribute(EffectAttribute::Duration).and_then(|a| m.attribute(a)).is_some_and(|v| (3000.0..=10000.0).contains(&v))
        && m.attribute(amount_attr).is_some_and(|v| v > 0.0)
        && m.attribute(effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED)).is_some_and(|v| v > 0.0)
        && (layer == 0 || m.attribute(ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER).is_some_and(|v| v > 1.0)))
        .find_map(|m| {
            let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter().flat_map(|n| sde.dogma.attributes_by_name(n))
                .filter_map(|a| m.attribute(a.attribute_id)).collect();
            let size = size_id.and_then(|id| m.attribute(id));
            let matches = |c: &crate::sde::types::TypeData| c.group_id.is_some_and(|group| groups.contains(&f64::from(group)))
                && size.is_none_or(|v| v <= 0.0 || size_id.and_then(|id| c.attribute(id)) == Some(v));
            let capacity = m.capacity.unwrap_or(0.0).max(m.attribute(ATTR_CAPACITY).unwrap_or(0.0));
            let charge = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && matches(c) && c.volume.is_some_and(|v| v > 0.0 && capacity >= v && capacity / v < f64::from(CARGO_QUANTITY)))?;
            let other = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE) && !matches(c))?;
            Some((m.type_id, charge.type_id, other.type_id))
        }).expect("published ancillary module and matching charge types");
    let module_data = sde.types.get(module_type).unwrap();
    let raw_amount = module_data.attribute(amount_attr).unwrap() * if layer == 1 && charged { module_data.attribute(ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER).unwrap() } else { 1.0 };
    let raw_need = module_data.attribute(effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED)).unwrap();
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE, ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE].iter().all(|&a| t.attribute(a).is_some_and(|v| v > 10.0))
        && t.attribute([ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP][layer]).is_some_and(|v| v > raw_amount)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v > raw_need)).expect("ship with sufficient health and capacitor").type_id;
    let system = 1;
    let before = fingerprint(&db).await.unwrap();
    let f = create(&db, ship_type, module_type, system).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        if charged {
            test_db.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, $2, $3, 1)", &[&f.ship, &FLAG_FITTING_FIRST, &charge_type]).await.unwrap();
        }
        test_db.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1, $2, $3)", &[&f.module, &ATTR_RELOAD_TIME, &RELOAD_MS]).await.unwrap();
        if reload {
            test_db.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, false)", &[&charge_type, &f.source, &f.ship, &FLAG_CARGO, &CARGO_QUANTITY]).await.unwrap();
        }
        let other: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, false) RETURNING item_id", &[&other_type, &f.source, &f.ship, &FLAG_CARGO, &OTHER_QUANTITY]).await.unwrap().get("item_id");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.source, session_id: f.source, language_id: "EN".into() };
        let mut client = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), test_world.clone()), test_world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(f.source)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(system))]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = tx;
        test_world.add_ship(system, ball(f.ship, ship_type, f.source)).await.unwrap();
        test_world.add_ship(system, ball(f.victim, ship_type, f.target)).await.unwrap();
        let stats = ship_stats(&test_db, &test_world, system, f.target, f.victim).await.ok().unwrap();
        state().condition.insert(f.victim, Condition { now: [0.0; 3], stats: stats.clone(), shield_at: filetime_now() + 60 * FILETIME_SECOND });
        state().targets.entry(f.source).or_default().insert(f.victim);
        let fit = Fit::load_in_space(&test_db, &test_world, system, f.source, f.ship).await.ok().unwrap();
        let base = fit.value(Key::Module(f.module), amount_attr);
        let bonus = fit.value(Key::Module(f.module), ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER);
        let expected = if layer == 1 && charged { base * bonus } else { base };
        assert!(stats.max[layer] > expected, "fixture must not clamp repair");
        let cap = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let need = fit.value(Key::Module(f.module), effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED));
        assert!(cap.is_finite() && need.is_finite() && cap > need && (need > 0.0 || (layer == 0 && charged && need == 0.0)), "invalid fixture capacitor: cap={cap}, need={need}");
        state().capacitor.insert(f.ship, (cap, filetime_now() + 60 * FILETIME_SECOND));
        let duration = fit.value(Key::Module(f.module), effect.attribute(EffectAttribute::Duration).unwrap()).max(250.0) as u64;
        let args = [Value::Int(f.module), Value::str(name), Value::Int(f.victim), Value::Int(1)];
        let kwargs = Value::Dict(vec![]);
        let call = Call { service: "dogmaIM", method: "Activate", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(crate::services::combat::dispatch(&mut client, &call).await.expect("combat method").is_ok());
        assert!(test_world.after(system, 1500).await);
        let remaining = crate::dao::items::charges::loaded(&test_db, f.ship, FLAG_FITTING_FIRST).await.unwrap();
        assert!(remaining.is_none(), "last charge consumed or magazine empty");
        let level = state().capacitor.get(&f.ship).unwrap().0;
        let expected_cap = if layer == 0 && charged { cap } else { cap - need };
        assert!((level - expected_cap).abs() < cap * 1e-8, "capacitor debit");
        assert!(test_world.after(system, duration + RELOAD_MS as u64 + 2500).await);
        let repaired = state().condition[&f.victim].now[layer];
        assert!((repaired - expected).abs() < expected.max(1.0) * 1e-8, "repair amount: {repaired} != {expected}");
        let loaded = crate::dao::items::charges::loaded(&test_db, f.ship, FLAG_FITTING_FIRST).await.unwrap();
        if reload {
            let loaded = loaded.expect("automatic reload from compatible cargo");
            assert_eq!(loaded.type_id, charge_type);
            assert!(loaded.quantity > 0);
        } else { assert!(loaded.is_none(), "incompatible cargo must not reload"); }
        let quantity: i32 = test_db.query_one("SELECT quantity FROM game.items WHERE item_id = $1", &[&other]).await.unwrap().get("quantity");
        assert_eq!(quantity, OTHER_QUANTITY, "incompatible cargo must not be consumed");
    }).await;
    // Let an activation or reload already scheduled before a panic finish before deleting rows.
    world.after(system, 15_000).await;
    world.remove_ball(system, f.ship).await;
    world.remove_ball(system, f.victim).await;
    let flush = world.flush_positions().await;
    forget_ship(f.ship);
    forget_ship(f.victim);
    state().targets.remove(&f.source);
    state().reloading.remove(&f.module);
    drop(world);
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id = $1", &[&f.ship]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id = $1", &[&f.ship]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&&[f.module, f.ship, f.victim][..]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE location_id = $1 AND flag_id = $2", &[&f.ship, &FLAG_CARGO]).await.unwrap();
    cleanup(&db, f).await.unwrap();
    assert_eq!(fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn ancillary_remote_armor_charged_db_smoke() { ancillary(EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER, 1, true, true).await; }
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn ancillary_remote_armor_empty_db_smoke() { ancillary(EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER, 1, false, false).await; }
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn ancillary_remote_shield_charged_db_smoke() { ancillary(EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER, 0, true, true).await; }
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn ancillary_remote_shield_empty_db_smoke() { ancillary(EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER, 0, false, false).await; }
