//! Real Thermodynamics skill modifiers and overload eligibility.
use super::*;
use infinity_core::{consts::{dogma::ATTR_SKILL_LEVEL, dogma::ATTR_SKILL_TIME_CONSTANT}};
use crate::{dao::skills::training, services::skills::formulas};

/// SDE Thermodynamics skill type.
const THERMODYNAMICS: i32 = 28164;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn thermodynamics_reduces_heat_damage_once_and_gates_overload() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(MEDIUM_CAPACITY).is_some_and(|v| v > 0.0)).expect("published heat-capable hull").type_id;
    let module = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.attribute(HEAT_DAMAGE).is_some_and(|v| v > 0.0)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && t.attribute(REQUIRED_THERMODYNAMICS).is_some_and(|v| v > 0.0 && v <= 5.0)
        && t.effects().iter().any(|e| i64::from(e.effect_id) == infinity_core::consts::dogma::EFFECT_ONLINE)
        && services::skills::requirements::requirements(&sde, t.type_id).all(|(skill, _)| skill != THERMODYNAMICS)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)
        && t.effects().iter().any(|e| e.effect_id == OVERLOAD_SPEED)).expect("overloadable propulsion module requiring Thermodynamics").type_id;
    let rank = sde.types.get(THERMODYNAMICS).and_then(|t| t.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).expect("Thermodynamics rank");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_MED_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let d = db.clone();
    let w = world.clone();
    let result = tokio::spawn(async move {
        fixture::position(&d, f.ship, SYSTEM, [0.0; 3]).await.unwrap();
        seed_module_prerequisites(&d, &sde, f.character, module).await;
        dogma::set_override(&d, f.module, HEAT_DAMAGE, 100.0).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), d.clone(), sde.clone(), crate::chat::Hub::new("localhost", d.clone(), w.clone()), w.clone(), login);
        c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = tx.clone();
        w.register_pilot(f.character, tx);
        w.system(SYSTEM).await.unwrap();
        let market = services::market::lock().await;
        let hull_row = crate::dao::items::space_ships::FreeShip { item_id: f.ship, type_id: hull, owner_id: f.character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = services::ship::balls::piloted(&d, sde.clone(), &hull_row, f.character, 0.0).await.ok().expect("prepare ship");
        w.with(SYSTEM, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        drop(market);
        combat::groups_health::set_damage(&c, SYSTEM, f.ship, f.character, [false, true, true], 0.25).await.ok().expect("damage armor and hull");
        let raw_damage = assert_cached_defenses(&d, &w, f.character, f.ship).await;
        let candidates: Vec<_> = sde.types.iter().filter(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SKILL) && t.published == Some(true))
            .filter(|t| t.type_id != THERMODYNAMICS && services::skills::requirements::requirements(&sde, module).all(|(skill, _)| skill != t.type_id))
            .filter(|t| [infinity_core::consts::dogma::ATTR_SHIELD_CAPACITY, infinity_core::consts::dogma::ATTR_ARMOR_HP, infinity_core::consts::dogma::ATTR_STRUCTURE_HP].into_iter()
                .any(|attr| t.modifiers(&sde.dogma, attr).any(|(_, m)| m.func == "ItemModifier" && m.domain == "shipID")))
            .map(|t| t.type_id).collect();
        let mut changed = false;
        for skill in candidates {
            services::skills::gm::apply(&mut c, f.character, services::skills::gm::Change::Set(vec![(skill, 1)])).await.ok().expect("set defensive skill");
            services::dogma::brain::update(&mut c).await.ok().expect("publish low-skill brain");
            let low = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap();
            let low_stats = combat::groups_health::ShipStats::from_fit(&low, Key::Ship);
            services::skills::gm::apply(&mut c, f.character, services::skills::gm::Change::Set(vec![(skill, 5)])).await.ok().expect("raise defensive skill");
            services::dogma::brain::update(&mut c).await.ok().expect("publish high-skill brain");
            let high = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap();
            let high_stats = combat::groups_health::ShipStats::from_fit(&high, Key::Ship);
            if low_stats.max != high_stats.max || low_stats.resonance != high_stats.resonance || low_stats.shield_tau_ms != high_stats.shield_tau_ms {
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        let current = w.with(SYSTEM, move |s| s.combat_condition_snapshot(f.ship)).await.flatten().unwrap();
                        if current.condition.stats.max == high_stats.max && current.condition.stats.resonance == high_stats.resonance
                            && current.condition.stats.shield_tau_ms == high_stats.shield_tau_ms { break; }
                        assert!(w.after(SYSTEM, 100).await);
                    }
                }).await.expect("owner brain publishes defensive fit");
                let actual_damage = assert_cached_defenses(&d, &w, f.character, f.ship).await;
                for i in 0..2 { assert!((actual_damage[i] - raw_damage[i]).abs() < 1e-8); }
                changed = true;
                break;
            }
        }
        assert!(changed, "SDE defensive skill must change fitted defenses");
        assert!(dogma::online(&d, f.ship).await.unwrap().iter().any(|m| m.item_id == f.module), "module remains online after defensive skill changes");
        let overload = [Value::Int(f.module), Value::Int(i64::from(OVERLOAD_SPEED))];
        let fit = Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap();
        assert_eq!(fit.value(Key::Module(f.module), HEAT_DAMAGE), 100.0);
        assert_eq!(fit.value(Key::Skill(THERMODYNAMICS), ATTR_SKILL_LEVEL), 0.0);
        let refusal = call(&mut c, "dogmaIM", "Overload", &overload, None).await;
        assert!(matches!(&refusal, Err(services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY
                && *dict == vec![("notify".into(), Value::unicode(infinity_core::messages::heat::THERMODYNAMICS_REQUIRED))]), "unexpected refusal: {refusal:?}");
        assert!(!heat::overloaded_modules().contains(&f.module));
        for (level, expected) in [(1, 95.0), (5, 75.0)] {
            // Invert the existing level calculation instead of duplicating its skill-point formula.
            let (mut low, mut high) = (0_i64, 1_i64 << 40);
            while low < high {
                let mid = low + (high - low) / 2;
                if formulas::level_from_points(mid, rank) < level { low = mid + 1; } else { high = mid; }
            }
            assert_eq!(formulas::level_from_points(low, rank), level);
            training::set_level(&d, f.character, THERMODYNAMICS, level, low).await.unwrap();
            services::dogma::brain::update(&mut c).await.expect("publish Thermodynamics brain");
            let fit = Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap();
            assert_eq!(fit.value(Key::Skill(THERMODYNAMICS), ATTR_SKILL_LEVEL), f64::from(level));
            assert_eq!(fit.value(Key::Module(f.module), HEAT_DAMAGE), expected);
        }
        call(&mut c, "dogmaIM", "Overload", &overload, None).await.ok().expect("trained Thermodynamics permits overload");
        assert!(heat::overloaded_modules().contains(&f.module));
    }).await;
    heat::clear_character(f.character);
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    training::remove(&db, f.character, &[THERMODYNAMICS]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn hardener_cycle_and_overload_publish_fitted_defenses() {
    use infinity_core::consts::dogma::{ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE,
        ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(MEDIUM_CAPACITY).is_some_and(|v| v > 0.0)).expect("heat-capable ship").type_id;
    let resonance = [ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE,
        ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE];
    let (module, active_name, overload, duration) = sde.types.iter()
        .filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE))
        .find_map(|t| {
            let active = t.effects().iter().find_map(|e| {
                let row = sde.dogma.effect(e.effect_id)?;
                let resolved = registry::resolve(&sde, e.effect_id)?;
                if resolved.kind != services::effects::EffectKind::Modifiers || !resolved.manually_activatable()
                    || !row.modifiers().iter().any(|m| m.domain == "shipID" && resonance.contains(&m.modified)) { return None; }
                Some((row.name.clone()?, resolved.attribute(EffectAttribute::Duration)?))
            })?;
            let overload = t.effects().iter().find_map(|e| {
                let row = sde.dogma.effect(e.effect_id)?;
                (row.effect_category == Some(5)).then_some(e.effect_id)
            })?;
            Some((t.type_id, active.0, overload, active.1))
        }).expect("overloadable defensive module with an active resonance modifier");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_MED_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let d = db.clone();
    let w = world.clone();
    let result = tokio::spawn(async move {
        fixture::position(&d, f.ship, SYSTEM, [0.0; 3]).await.unwrap();
        for (item, attr, value) in [
            (f.ship, ATTR_MED_SLOTS, 1.0), (f.ship, ATTR_CPU_OUTPUT, 1_000_000.0),
            (f.ship, ATTR_POWER_OUTPUT, 1_000_000.0), (f.ship, ATTR_CAPACITOR_CAPACITY, 1_000_000.0),
            (f.module, ATTR_CPU, 0.0), (f.module, ATTR_POWER, 0.0), (f.module, ATTR_CAPACITOR_NEED, 0.0),
            (f.module, REQUIRED_THERMODYNAMICS, 0.0), (f.module, HEAT_DAMAGE, 0.0), (f.module, duration, 2000.0),
        ] { dogma::set_override(&d, item, attr, value).await.unwrap(); }
        for attr in ATTR_REQUIRED_SKILL_LEVELS { dogma::set_override(&d, f.module, attr, 0.0).await.unwrap(); }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), d.clone(), sde.clone(), crate::chat::Hub::new("localhost", d.clone(), w.clone()), w.clone(), login);
        c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = tx.clone();
        w.register_pilot(f.character, tx);
        w.system(SYSTEM).await.unwrap();
        let market = services::market::lock().await;
        let row = crate::dao::items::space_ships::FreeShip { item_id: f.ship, type_id: hull, owner_id: f.character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = services::ship::balls::piloted(&d, sde.clone(), &row, f.character, 0.0).await.ok().unwrap();
        w.with(SYSTEM, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        drop(market);
        combat::groups_health::set_damage(&c, SYSTEM, f.ship, f.character, [false, true, true], 0.25).await.ok().unwrap();
        let raw = assert_cached_defenses(&d, &w, f.character, f.ship).await;
        let baseline = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap();
        let baseline_stats = combat::groups_health::ShipStats::from_fit(&baseline, Key::Ship);
        let activation = [Value::Int(f.module), Value::str(&active_name), Value::None, Value::Int(10)];
        call(&mut c, "dogmaIM", "Activate", &activation, None).await.ok().expect("activate hardener");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot = w.with(SYSTEM, move |s| s.combat_condition_snapshot(f.ship)).await.flatten().unwrap();
                if snapshot.condition.stats.resonance != baseline_stats.resonance { break; }
                assert!(w.after(SYSTEM, 100).await);
            }
        }).await.expect("active hardener publishes resonance");
        let active_fit = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap();
        let active_stats = combat::groups_health::ShipStats::from_fit(&active_fit, Key::Ship);
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw);
        let overload_args = [Value::Int(f.module), Value::Int(i64::from(overload))];
        call(&mut c, "dogmaIM", "Overload", &overload_args, None).await.ok().expect("overload hardener");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !heat::overloaded_modules().contains(&f.module) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("hardener overload boundary");
        let hot_fit = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap();
        let hot_stats = combat::groups_health::ShipStats::from_fit(&hot_fit, Key::Ship);
        assert_ne!(hot_stats.resonance, active_stats.resonance, "selected overload must change defenses");
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw);
        call(&mut c, "dogmaIM", "StopOverload", &overload_args, None).await.ok().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while heat::overloaded_modules().contains(&f.module) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("hardener overload stop boundary");
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw);
        call(&mut c, "dogmaIM", "Deactivate", &[Value::Int(f.module), Value::str(&active_name)], None).await.ok().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while combat::state().active.contains_key(&f.module) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("hardener retired");
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw);
    }).await;
    { let mut st = combat::state(); if let Some(active) = st.active.remove(&f.module) { active.stop.store(true, std::sync::atomic::Ordering::Relaxed); } st.activation_generation.remove(&f.module); }
    heat::clear_character(f.character);
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
