//! Real-database regression for capacitor booster charge consumption and injection.
use super::*;
use crate::services::combat::groups_health::tests::drain_exact;
use std::sync::{Arc, atomic::AtomicBool};
use infinity_core::{
    consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CHARGE, dogma::ATTR_QUANTITY, dogma::ATTR_RECHARGE_RATE, CATEGORY_CHARGE, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_POWER_BOOSTER, FLAG_FITTING_FIRST},
};
use crate::{
    db::{Db, Error},
    services::{combat, effects::{lifecycle::{ActivationOwner, CycleOutcome}, registry}},
    space::{Ball, World, WarpState, MODE_STOP},
};

#[derive(Clone, Copy)]
struct Fixture { account: i64, character: i64, ship: i64, module: i64 }

async fn create(db: &Db, ship_type: i32, module_type: i32, charge_type: i32, system: i64) -> Result<Fixture, Error> {
    db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('booster-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'booster-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
            let ship = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES ($1, $2, $3, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&ship_type, &character, &system]).await?.get("item_id");
            let module = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, online) VALUES ($1, $2, $3, $4, 1, true, true) RETURNING item_id", &[&module_type, &character, &ship, &FLAG_FITTING_FIRST]).await?.get("item_id");
            tx.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, $2, $3, 2)", &[&ship, &FLAG_FITTING_FIRST, &charge_type]).await?;
            Ok(Fixture { account, character, ship, module })
        }), |fixture, ()| async move { Ok(fixture) },
    ).await
}

async fn cleanup(db: &Db, f: Fixture) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&f.ship, &FLAG_FITTING_FIRST]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2 AND location_id = $3", &[&f.module, &f.character, &f.ship]).await?;
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
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY ship_id, flag_id), '')) AS fingerprint FROM game.module_charges t",
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

fn change(event: (String, Vec<Value>), owner: i64, item: Value, attr: i32) -> (Value, Value) {
    assert_eq!(event.0, "OnModuleAttributeChanges");
    let [Value::List(changes)] = event.1.as_slice() else { panic!("expected attribute changes") };
    let [Value::Tuple(row)] = changes.as_slice() else { panic!("expected one change") };
    assert_eq!(row.len(), 8);
    assert_eq!(&row[..4], &[Value::str("OnModuleAttributeChange"), Value::Int(owner), item, Value::Int(i64::from(attr))]);
    (row[5].clone(), row[6].clone())
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn cap_booster_db_smoke() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, EFFECT_POWER_BOOSTER).expect("power booster effect in SDE");
    assert!(matches!(effect.kind, EffectKind::CapBooster));
    let size_id = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let (module_type, charge_type) = sde.types.iter().filter(|m| m.published == Some(true)
        && m.category_id == Some(CATEGORY_MODULE) && m.effects().iter().any(|e| e.effect_id == EFFECT_POWER_BOOSTER))
        .find_map(|m| {
            let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
                .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| m.attribute(a.attribute_id)).collect();
            let module_size = size_id.and_then(|id| m.attribute(id));
            sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && c.group_id.is_some_and(|g| groups.contains(&f64::from(g)))
                && (module_size.is_none_or(|v| v <= 0.0) || size_id.and_then(|id| c.attribute(id)) == module_size)
                && c.attribute(ATTR_CAPACITOR_BONUS).is_some_and(|v| v.is_finite() && v > 0.0))
                .map(|c| (m.type_id, c.type_id))
        }).expect("published booster and compatible capacitor charge");
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_RECHARGE_RATE).is_some_and(|v| v.is_finite() && v > 0.0)).expect("ship with capacitor").type_id;
    let system = 1;
    let before = fingerprint(&db).await.unwrap();
    let fixture = create(&db, ship_type, module_type, charge_type, system).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(fixture.character, tx.clone());
        test_world.add_ship(system, ball(fixture.ship, ship_type, fixture.character)).await.unwrap();
        let fit = Fit::load(&test_db, sde, fixture.character, fixture.ship).await.ok().expect("load fit");
        let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let bonus = fit.value(Key::Charge(fixture.module), ATTR_CAPACITOR_BONUS);
        assert!(capacity.is_finite() && capacity > 0.0 && bonus.is_finite() && bonus > 0.0);
        let empty_type = test_world.sde().types.iter().find(|ty| ty.attribute(ATTR_CAPACITOR_CAPACITY).unwrap_or(0.0) <= 0.0).expect("SDE type with no capacitor").type_id;
        let mut empty = ball(-9_001_001, empty_type, fixture.character);
        empty.char_id = None;
        test_world.add_ship(system, empty).await.unwrap();
        let admitted = NpcCapacitorGuard::Attack {
            source: fixture.ship, target: -9_001_001, source_guard: None, player: None,
            optimal: 100.0, falloff: 0.0, offensive: false,
        };
        assert_eq!(drain_exact_guarded(&test_db, &test_world, system, -9_001_001, 10.0, admitted).await.ok().unwrap().unwrap().0, 0.0);
        test_world.remove_ball(system, -9_001_001).await;
        combat::forget_ship(-9_001_001);
        let stale = NpcCapacitorGuard::Attack {
            source: fixture.ship, target: fixture.ship,
            source_guard: Some(crate::services::effects::space_source::SourceGuard::Drone { source: fixture.ship, target: fixture.ship, generation: 0 }),
            player: None, optimal: 100.0, falloff: 0.0, offensive: true,
        };
        assert!(drain_exact_guarded(&test_db, &test_world, system, fixture.ship, 0.0, stale).await.ok().unwrap().is_none());
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: fixture.character, ship_id: fixture.ship, system_id: system };
        let module = Module { id: fixture.module, type_id: module_type, flag: FLAG_FITTING_FIRST };
        let stop = Arc::new(AtomicBool::new(false));
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![module], effect, target: None, repeat: 1,
            stop: stop.clone(), owner: ActivationOwner { master: fixture.module, generation: stop, armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 0.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        assert_eq!(loaded(&test_db, fixture.ship, FLAG_FITTING_FIRST).await.ok().unwrap(), Some((charge_type, 2)));
        assert!(validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.is_ok());
        for remaining in [1, 0] {
            assert!(drain_exact(&test_db, &test_world, system, fixture.ship, capacity * 0.75).await.ok().unwrap() > 0.0);
            let _ = rx.try_recv().expect("drain notification");
            assert_eq!(start(&cx, &fit, &mut cycle).await.ok().expect("booster start"), CycleOutcome::Continue);
            assert_eq!(loaded(&test_db, fixture.ship, FLAG_FITTING_FIRST).await.ok().unwrap(), if remaining == 0 { None } else { Some((charge_type, remaining)) });
            let key = Value::tuple([Value::Int(fixture.ship), Value::Int(i64::from(FLAG_FITTING_FIRST)), Value::Int(i64::from(charge_type))]);
            let (new, old) = change(rx.try_recv().expect("quantity notification"), fixture.character, key, ATTR_QUANTITY);
            assert_eq!((new, old), (Value::Int(i64::from(remaining)), Value::Int(i64::from(remaining + 1))));
            let (new, old) = change(rx.try_recv().expect("capacitor notification"), fixture.character, Value::Int(fixture.ship), ATTR_CHARGE);
            let (Value::Float(new), Value::Float(old)) = (new, old) else { panic!("expected capacitor values") };
            assert!(old < capacity);
            assert!((new - (old + bonus).min(capacity)).abs() <= capacity * 1e-10);
        }
        assert!(validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.is_err());
        assert_eq!(start(&cx, &fit, &mut cycle).await.ok().expect("empty booster start"), CycleOutcome::Stop("out of charges"));
        assert!(rx.try_recv().is_err());
    }).await;
    world.remove_ball(system, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    let flush = world.flush_positions().await;
    combat::forget_ship(fixture.ship);
    drop(world);
    cleanup(&db, fixture).await.unwrap();
    assert_eq!(fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
