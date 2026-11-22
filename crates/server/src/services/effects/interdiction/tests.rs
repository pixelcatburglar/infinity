//! Database-backed actor scenario for the unscripted warp-disruption sphere:
//! a real Activate raises the actor's field and its client animation, the
//! generator's own-ship penalties apply while it runs, and hulls carrying
//! `warpBubbleImmune` pass through untouched. An assistance projection
//! already on the source stops applying while a generator runs and resumes
//! when it retires.
use super::super::tests::fixture;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipFixture, ShipItemSpec, ShipLocation};
use crate::services::combat;
use crate::dao::items::dogma as item_dogma;
use crate::services::effects::{lifecycle, projection::Projection, registry, EffectKind, JammingType};
use crate::services::fit::Key;
use crate::services::identity::Identity;
use crate::space::World;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, GROUP_HEAVY_INTERDICTORS,
    dogma::{ATTR_CPU, ATTR_CPU_OUTPUT, ATTR_HI_SLOTS, ATTR_POWER, ATTR_POWER_OUTPUT,
    ATTR_SIGNATURE_RADIUS, ATTR_SCAN_RESOLUTION, ATTR_WARP_BUBBLE_IMMUNE,
    EFFECT_REMOTE_SENSOR_BOOST, EFFECT_WARP_DISRUPT_SPHERE, EFFECT_WARP_SCRAMBLE}};
use marshal::Value;
use std::sync::Arc;

/// SDE type: Warp Disruption Field Generator I.
const WDFG_I: i32 = 28654;
/// SDE dgmEffects guid of the unscripted sphere's field animation.
const FIELD_GUID: &str = "effects.WarpDisruptFieldGenerating";

/// SDE type: Focused Warp Disruption Script.
const FOCUSED_DISRUPT_SCRIPT: i32 = 29003;

/// `OnGodmaShipEffect` start flags for (module, warpDisruptSphere).
fn godma(rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, module: i64) -> Vec<Value> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|(name, args)| name == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(module))
            && args.get(1) == Some(&Value::Int(i64::from(EFFECT_WARP_DISRUPT_SPHERE))))
        .filter_map(|(_, args)| args.get(3).cloned()).collect()
}

/// Unpack one destiny observer's batches into `OnSpecialFX` argument tuples.
async fn field_fx(events: &mut tokio::sync::mpsc::UnboundedReceiver<crate::space::SceneDelivery>, module: i64) -> Vec<Value> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let crate::space::SceneDelivery::Destiny(batch) = events.recv().await.expect("destiny observer") else { continue };
            for event in batch {
                let Value::Tuple(envelope) = event else { continue };
                let Some(Value::Tuple(call)) = envelope.get(1) else { continue };
                if call.first() != Some(&Value::str("OnSpecialFX")) { continue; }
                let Some(Value::Tuple(args)) = call.get(1) else { continue };
                if args.get(1) == Some(&Value::Int(module)) { return args.clone(); }
            }
        }
    }).await.expect("field animation")
}

/// One generator fixture: a published heavy-interdictor hull legally fitting
/// `count` online generators, with the module's and hull's required skills
/// seeded through the existing GM path before admission.
async fn generator(sde: &Arc<crate::sde::Sde>, db: &crate::db::Db, system: i64, count: usize) -> (ShipFixture, i32) {
    let module = sde.types.get(WDFG_I).expect("Warp Disruption Field Generator I");
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.group_id == Some(GROUP_HEAVY_INTERDICTORS) && t.published == Some(true)
        && t.attribute(ATTR_HI_SLOTS).is_some_and(|slots| slots >= count as f64)
        && module.attribute(ATTR_CPU).zip(t.fit_attribute(ATTR_CPU_OUTPUT)).is_some_and(|(need, out)| out >= need * count as f64)
        && module.attribute(ATTR_POWER).zip(t.fit_attribute(ATTR_POWER_OUTPUT)).is_some_and(|(need, out)| out >= need * count as f64))
        .map(|t| t.type_id).expect("published heavy interdictor legally fitting the generators");
    let specs = (0..count).map(|slot| ShipItemSpec { type_id: WDFG_I, flag_id: FLAG_HI_SLOT_FIRST + slot as i32,
        quantity: 1, singleton: true, online: true, attributes: &[] }).collect::<Vec<_>>();
    let fixture = create_ship(db, hull, ShipLocation::Space { system }, &specs).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let required = [WDFG_I, hull].into_iter().flat_map(|t| crate::services::skills::requirements::requirements(sde, t))
        .fold(std::collections::BTreeMap::new(), |mut map, (skill, level)| { map.entry(skill).and_modify(|v: &mut i32| *v = (*v).max(level)).or_insert(level); map });
    crate::services::skills::gm::apply(&mut client, fixture.character, crate::services::skills::gm::Change::Set(required.into_iter().collect())).await.unwrap();
    (fixture, hull)
}

/// The filtered incoming projection module ids on `ship` plus one attribute's
/// current effective value on its resident fit.
async fn observed(world: &World, system: i64, ship: i64, attr: i32) -> (Vec<i64>, f64) {
    let (modules, value) = world.with(system, move |s| (
        s.projections_on(ship).iter().map(|p| p.source_module).collect::<Vec<_>>(),
        s.with_combat_fit(ship, |fit, _, _| fit.value(Key::Ship, attr)),
    )).await.unwrap();
    (modules, value.expect("resident combat fit"))
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn active_sphere_filters_existing_assistance_and_retirement_restores() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let zero = sde.map.systems().find(|s| s.security.is_some_and(|v| v <= 0.0)).expect("SDE null-security system").solar_system_id;
    let (source, hull) = generator(&sde, &db, zero, 2).await;
    let (bare, scripted) = (source.items[0], source.items[1]);
    crate::dao::items::combat::apply_loads(&db, source.ship, &[(scripted, FLAG_HI_SLOT_FIRST + 1, FOCUSED_DISRUPT_SCRIPT, 1, 1)]).await.unwrap();
    let duration_attr = sde.dogma.effect(EFFECT_WARP_DISRUPT_SPHERE)
        .and_then(|e| e.duration_attribute_id).expect("sphere duration attribute");
    item_dogma::set_override(&db, bare, duration_attr, 1_500.0).await.unwrap();
    // A real assistance-flagged projection whose scan-resolution bonus shows
    // on the receiving fit, plus the two rows the filter must never drop.
    let boost = sde.dogma.effect(EFFECT_REMOTE_SENSOR_BOOST).expect("sensor boost effect");
    assert!(boost.is_assistance);
    let modifying = boost.modifiers().iter().find(|m| m.domain == "targetID" && m.modified == ATTR_SCAN_RESOLUTION)
        .expect("boost scan-resolution modifier").modifying;
    let target = create_ship(&db, hull, ShipLocation::Space { system: zero }, &[]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(source.character, tx.clone());
    fixture::admit(&db, &world, zero, fixture::ball(source.ship, hull, source.character, Some(source.character), 0.0)).await;
    fixture::admit(&db, &world, zero, fixture::ball(target.ship, hull, target.character, Some(target.character), 5_000.0)).await;
    let identity = Identity { char_id: source.character, ship_id: Some(source.ship), corp_id: 1, station_id: None, system_id: Some(zero) };
    let result = crate::tests::catch_scenario(async {
        let baseline = observed(&world, zero, source.ship, ATTR_SCAN_RESOLUTION).await.1;
        let projection = move |source_ship, source_module, effect_id, kind, values| Projection {
            source_ship, source_module, source_type: hull, effect_id, kind, until_ms: i64::MAX,
            values, charge_values: vec![], charge_type: None, resistance_attribute: None, scramble: None };
        world.with(zero, move |s| {
            s.upsert_projection(source.ship, projection(target.ship, -11, EFFECT_REMOTE_SENSOR_BOOST,
                EffectKind::Projected { jamming: JammingType::SensorBoost }, vec![(modifying, 50.0)]));
            s.upsert_projection(source.ship, projection(source.ship, -12, EFFECT_REMOTE_SENSOR_BOOST,
                EffectKind::Projected { jamming: JammingType::SensorBoost }, vec![]));
            s.upsert_projection(source.ship, projection(target.ship, -13, EFFECT_WARP_SCRAMBLE,
                EffectKind::Projected { jamming: JammingType::Scramble }, vec![]));
        }).await.unwrap();
        let boosted = observed(&world, zero, source.ship, ATTR_SCAN_RESOLUTION).await.1;
        assert_ne!(boosted, baseline, "the assistance projection must land on the effective fit");
        // Unscripted and scripted activations both present warpDisruptSphere:
        // the first published active bit filters foreign assistance and each
        // retirement restores the same unexpired projection, always through
        // the real lifecycle, never a manual projection refresh.
        for (module, target_id) in [(bare, None), (scripted, Some(target.ship))] {
            if let Some(target_id) = target_id {
                crate::services::combat::tests::remote_repair_db::set_locked_target(source.character, target_id, true);
            }
            lifecycle::activate(&identity, &db, &world, &sde, Some(zero), tx.clone(), source.account,
                module, "warpDisruptSphere", target_id, 1_000, |_| Ok(())).await.unwrap();
            assert!(combat::state().active.contains_key(&module), "activation accepted");
            for _ in 0..20 {
                if !observed(&world, zero, source.ship, ATTR_SCAN_RESOLUTION).await.0.contains(&-11) { break; }
                assert!(world.after(zero, 100).await);
            }
            let (modules, value) = observed(&world, zero, source.ship, ATTR_SCAN_RESOLUTION).await;
            assert_eq!((modules.contains(&-12), modules.contains(&-13), modules.contains(&-11)), (true, true, false),
                "sphere {module} must keep self-source and harmful rows while dropping foreign assistance");
            assert_eq!(value, baseline, "blocked assistance must leave the effective fit");
            lifecycle::deactivate(&identity, &db, &world, &sde, Some(zero), tx.clone(), source.account,
                module, "warpDisruptSphere", |_| Ok(())).await.unwrap();
            for _ in 0..30 {
                if !combat::state().active.contains_key(&module) { break; }
                assert!(world.after(zero, 500).await);
            }
            assert!(!combat::state().active.contains_key(&module));
            let (modules, value) = observed(&world, zero, source.ship, ATTR_SCAN_RESOLUTION).await;
            assert!(modules.contains(&-11) && value == boosted,
                "retirement must restore the same unexpired assistance projection");
        }
    }).await;
    crate::services::combat::tests::remote_repair_db::set_locked_target(source.character, target.ship, false);
    for ship in [source.ship, target.ship] { world.remove_ball(zero, ship).await; combat::forget_ship(ship); }
    world.unregister_pilot_generation(source.character, None);
    {
        let mut state = combat::state();
        for item in [bare, scripted] { state.active.remove(&item); state.activation_generation.remove(&item); }
    }
    for fixture in [source, target] { cleanup_ship(&db, Some(&world), fixture).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn sphere_activation_publishes_field_fx_and_restores_on_stop() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let zero = sde.map.systems().find(|s| s.security.is_some_and(|v| v <= 0.0)).expect("SDE null-security system").solar_system_id;
    let (source, hull) = generator(&sde, &db, zero, 1).await;
    let module_item = source.items[0];
    // The real SDE cycle is thirty seconds; bound it like the existing
    // duration fixtures so this scenario measures the lifecycle only.
    let duration_attr = sde.dogma.effect(EFFECT_WARP_DISRUPT_SPHERE)
        .and_then(|e| e.duration_attribute_id).expect("sphere duration attribute");
    item_dogma::set_override(&db, module_item, duration_attr, 1_500.0).await.unwrap();
    // A ship inside the field is caught; a hull with a positive warpBubbleImmune
    // type attribute passes through it untouched.
    let victim = create_ship(&db, hull, ShipLocation::Space { system: zero }, &[]).await.unwrap();
    let immune_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_WARP_BUBBLE_IMMUNE).is_some_and(|v| v > 0.0))
        .map(|t| t.type_id).expect("ship type carrying warpBubbleImmune");
    let immune = create_ship(&db, immune_type, ShipLocation::Space { system: zero }, &[]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(source.character, tx.clone());
    let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
    fixture::admit(&db, &world, zero, fixture::ball(source.ship, hull, source.character, Some(source.character), 0.0)).await;
    fixture::admit(&db, &world, zero, fixture::ball(victim.ship, hull, victim.character, Some(victim.character), 5_000.0)).await;
    fixture::admit(&db, &world, zero, fixture::ball(immune.ship, immune_type, immune.character, Some(immune.character), 6_000.0)).await;
    world.observe(zero, source.character, source.ship, destiny).await.unwrap();
    let effect = registry::resolve(&sde, EFFECT_WARP_DISRUPT_SPHERE).expect("sphere effect resolves");
    let range_attr = effect.attribute(registry::EffectAttribute::Range).expect("sphere range attribute");
    let signature_bonus = sde.dogma.attributes_by_name("signatureRadiusBonus").next()
        .map(|a| a.attribute_id).expect("signatureRadiusBonus attribute");
    let (radius, base_signature) = world.with(zero, move |s| s.with_combat_fit(source.ship, |fit, _, _| (
        fit.value(Key::Module(module_item), range_attr), fit.value(Key::Ship, ATTR_SIGNATURE_RADIUS)))).await.flatten().unwrap();
    let identity = Identity { char_id: source.character, ship_id: Some(source.ship), corp_id: 1, station_id: None, system_id: Some(zero) };
    let result = crate::tests::catch_scenario(async {
        assert!(lifecycle::activate(&identity, &db, &world, &sde, Some(zero), tx.clone(), source.account, module_item,
            "warpDisruptSphere", None, 1_000, |_| Ok(())).await.is_ok());
        assert!(combat::state().active.contains_key(&module_item), "activation accepted");
        // The field rises once inside the deferred begin work; poll until the
        // actor admits it rather than asserting after a fixed sleep.
        for _ in 0..20 {
            if world.with(zero, move |s| s.interdicted(victim.ship)).await.unwrap_or(false) { break; }
            assert!(world.after(zero, 100).await);
        }
        let (caught, passed) = world.with(zero, move |s| (s.interdicted(victim.ship), s.interdicted(immune.ship))).await.unwrap();
        assert!(caught && !passed, "field catches the plain ship and spares the immune one");
        let (signature, bonus) = world.with(zero, move |s| s.with_combat_fit(source.ship, |fit, _, _| (
            fit.value(Key::Ship, ATTR_SIGNATURE_RADIUS), fit.value(Key::Module(module_item), signature_bonus))))
            .await.flatten().unwrap();
        let expected = base_signature * (1.0 + bonus / 100.0);
        assert!((signature - expected).abs() <= f64::EPSILON * expected.abs(),
            "signatureRadius {signature} is not base {base_signature} scaled by PostPercent {bonus} ({expected})");
        let args = field_fx(&mut events, module_item).await;
        assert_eq!(args.get(0), Some(&Value::Int(source.ship)));
        assert_eq!(args.get(2), Some(&Value::Int(i64::from(WDFG_I))));
        assert_eq!(args.get(5), Some(&Value::str(FIELD_GUID)));
        assert_eq!(args.get(13), Some(&crate::services::keyval([("range", Value::Float(radius))])));
        assert_eq!(godma(&mut rx, module_item), vec![Value::Int(1)]);
        assert!(lifecycle::deactivate(&identity, &db, &world, &sde, Some(zero), tx.clone(), source.account,
            module_item, "warpDisruptSphere", |_| Ok(())).await.is_ok());
        // Retirement rides the end of the current cycle: the field and its one
        // stop notification land on the same boundary.
        for _ in 0..30 {
            if !combat::state().active.contains_key(&module_item) { break; }
            assert!(world.after(zero, 500).await);
        }
        assert!(!combat::state().active.contains_key(&module_item));
        assert!(!world.with(zero, move |s| s.interdicted(victim.ship)).await.unwrap());
        assert_eq!(godma(&mut rx, module_item), vec![Value::Int(0)]);
        let signature = world.with(zero, move |s| s.with_combat_fit(source.ship, |fit, _, _| fit.value(Key::Ship, ATTR_SIGNATURE_RADIUS)))
            .await.flatten().unwrap();
        assert_eq!(signature, base_signature, "self-penalty restores with the field");
    }).await;
    world.unobserve(source.character);
    for ship in [source.ship, victim.ship, immune.ship] { world.remove_ball(zero, ship).await; combat::forget_ship(ship); }
    world.unregister_pilot_generation(source.character, None);
    {
        let mut state = combat::state();
        state.active.remove(&module_item);
        state.activation_generation.remove(&module_item);
    }
    for fixture in [source, victim, immune] { cleanup_ship(&db, Some(&world), fixture).await.unwrap(); }
    result.unwrap();
}
