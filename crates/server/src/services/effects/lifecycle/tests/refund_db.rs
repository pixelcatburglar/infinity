use super::*;
use infinity_core::{consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_RECHARGE_RATE, CATEGORY_SHIP}};
use crate::{dao::items::{combat, pending_reloads}, services::combat::{groups_health::{capacitor_snapshot, drain_capacitor}, targeting_charges::loaded}, space::World};
use crate::services::effects::weapons::consume_charge;
use crate::services::fit::Fit;
use crate::space::combat::loaded::LoadedSeed;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn refused_charge_consumption_refunds_debit() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_RECHARGE_RATE).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let (module_type, charge_type) = crate::services::combat::targeting_charges::tests::compatible_ammunition(&sde);
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let fixture = crate::services::tests::fixture::create_ship(&db, ship_type,
        crate::services::tests::fixture::ShipLocation::Docked { station: 1 },
        &[crate::services::tests::fixture::ShipItemSpec { type_id: module_type, flag_id: flag, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = fixture.module();
    let (character, ship) = (fixture.character, fixture.ship);
    combat::apply_loads(&db, ship, &[(module_id, flag, charge_type, 1, 0)]).await.unwrap();
    let reload = pending_reloads::insert(&db, character, ship, module_id, flag, charge_type, 1, infinity_core::consts::FLAG_CARGO, ship).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        w.register_pilot(character, tx.clone());
        let ball = crate::services::effects::tests::fixture::ball(ship, ship_type, character, Some(character), 0.0);
        w.add_ship(1, ball).await.unwrap();
        let cx = Ctx { db: d.clone(), world: w.clone(), notify: tx, char_id: character, ship_id: ship, system_id: 1 };
        let fit = Fit::load(&d, sde.clone(), character, ship).await.ok().expect("load fit");
        let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let debit = capacity / 2.0;
        let tau = fit.value(Key::Ship, ATTR_RECHARGE_RATE);
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
        w.with(1, move |s| s.install_condition(ship, crate::services::combat::groups_health::Condition { now: stats.max, stats, shield_at: 0 }, fit)).await.unwrap();
        assert!(drain_capacitor(&cx, capacity, tau, debit));
        let permit = w.with(1, move |s| s.hold_loaded_use(ship, module_id)).await.flatten();
        assert_eq!(consume_charge(&cx, permit, None).await, None, "a pending reload refuses the use");
        assert!(!w.with(1, move |s| s.loaded_empty_proof(ship, module_id)).await.unwrap(), "a pending reload also blocks the empty proof");
        refund_refused_charge(&cx, debit).await.ok().expect("refund capacitor");
        let level = capacitor_snapshot(&d, &w, 1, ship).await.ok().expect("snapshot capacitor").unwrap().level;
        assert!((level - capacity).abs() < 1e-6);
        assert_eq!(loaded(&d, ship, flag).await.ok().expect("read loaded charge"), Some((charge_type, 1)));
        pending_reloads::take(&d, reload).await.unwrap();
        // Clearing the durable reservation alone does not release the slot.
        let membership = w.with(1, move |s| s.combat_membership_generation(ship)).await.flatten().expect("resident membership");
        assert!(w.with(1, move |s| s.publish_loaded_absolute(ship, membership, vec![LoadedSeed {
            module: module_id, flag, type_id: Some(charge_type), quantity: 1, pending: false }])).await.unwrap());
        // A token naming a different charge is refused like a type drift.
        let permit = w.with(1, move |s| s.hold_loaded_use(ship, module_id)).await.flatten().expect("admitted use");
        let token = crate::space::combat::inputs::ChargeToken { ship, membership: 0, module: module_id, incarnation: 0, type_id: charge_type + 1 };
        assert_eq!(consume_charge(&cx, Some(permit), Some(token)).await, None, "mismatched charge refuses");
        assert_eq!(loaded(&d, ship, flag).await.ok().expect("read loaded charge"), Some((charge_type, 1)));
        // A stopped cycle returns an unconsumed loaded admission: the slot's
        // finite count drops and the same use can be admitted again.
        let effect = crate::services::effects::registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_POWER_BOOSTER).expect("booster effect");
        let stop = Arc::new(AtomicBool::new(false));
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: module_type, flag }], effect,
            target: None, repeat: 1, stop: stop.clone(),
            owner: ActivationOwner { master: module_id, generation: stop, armed: false,
                combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 0.0, nominal_start: 0, other: None,
            repair_amount: 0.0, illegal: false, source_membership: None, target_type: None, inputs: Default::default() };
        let released = w.with(1, move |s| {
            assert!(crate::services::effects::lifecycle::capture_cycle_inputs(s, &mut cycle).unwrap());
            let held = s.loaded_outstanding();
            crate::services::effects::lifecycle::end::cycle_stop(s, cycle);
            let readmit = s.hold_loaded_use(ship, module_id);
            if let Some(readmit) = readmit { s.release_loaded_use(readmit); }
            (held, s.loaded_outstanding(), readmit.is_some())
        }).await.unwrap();
        assert_eq!(released, (true, false, true), "a stopped cycle returns its loaded admission");
    }).await;
    world.remove_ball(1, ship).await;
    world.unregister_pilot_generation(character, None);
    world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(ship);
    pending_reloads::take(&db, reload).await.unwrap();
    combat::delete_charge(&db, ship, flag).await.unwrap();
    crate::services::tests::fixture::cleanup_ship(&db, None, fixture).await.unwrap();
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
    let fixture = crate::services::tests::fixture::create_ship(&db, ship_type, crate::services::tests::fixture::ShipLocation::Docked { station: 1 }, &[crate::services::tests::fixture::ShipItemSpec { type_id: module_type, flag_id: flag, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = fixture.module();
    let world = World::new(db.clone(), sde, 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let system = 1;
    let result = tokio::spawn(async move {
        combat::apply_loads(&test_db, fixture.ship, &[(module_id, flag, charge_type, 2, 0)]).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(fixture.character, tx.clone());
        let ball = crate::services::effects::tests::fixture::ball(fixture.ship, ship_type, fixture.character, Some(fixture.character), 0.0);
        crate::services::effects::tests::fixture::admit(&test_db, &test_world, system, ball).await;
        while rx.try_recv().is_ok() {}
        let ship = fixture.ship;
        let token = test_world.with(system, move |s| s.fitted_charge_token(ship, module_id))
            .await.flatten().expect("resident charge");
        assert_eq!(token.type_id, charge_type);
        let revision = test_world.with(system, move |s| s.combat_fit_revision(ship)).await.unwrap().expect("installed actor fit");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: fixture.character, ship_id: ship, system_id: system };
        let permit = test_world.with(system, move |s| s.hold_loaded_use(ship, module_id)).await.flatten().expect("admitted use");
        assert_eq!(consume_charge(&cx, Some(permit), Some(token)).await, Some(charge_type));
        assert_eq!(test_world.with(system, move |s| s.loaded_effective(ship, module_id)).await.flatten().map(|v| (v.1, v.2)), Some((charge_type, 1)));
        test_world.flush_positions().await.unwrap();
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
        let remaining = test_world.with(system, move |s| s.fitted_charge_token(ship, module_id))
            .await.flatten().expect("positive charge remains resident");
        assert_eq!((remaining.membership, remaining.incarnation, remaining.type_id),
            (token.membership, token.incarnation, token.type_id));
        let permit = test_world.with(system, move |s| s.hold_loaded_use(ship, module_id)).await.flatten().expect("last round admitted");
        assert_eq!(consume_charge(&cx, Some(permit), Some(remaining)).await, Some(charge_type));
        test_world.flush_positions().await.unwrap();
        assert_eq!(loaded(&test_db, ship, flag).await.unwrap(), None);
        let final_state = test_world.with(system, move |s| s.with_combat_fit(ship, move |fit, _, stamp| (
            stamp.membership, fit.source_type(Key::Charge(module_id)), fit.charge_incarnation(module_id),
        ))).await.flatten().expect("same resident actor");
        assert_eq!(final_state, (token.membership, None, None));
        // Emptying the slot also removes the resident charge node, so the
        // module's charge-modified attributes revert in a first fitting
        // event; the emptied quantity row lands in the following event. The
        // expected attributes are the picked charge's modifiers the
        // evaluator applies to the loaded module: from a charge source
        // `otherID` resolves to the module's key (services/fit.rs
        // `modifier_applies`), every other domain binds another item, and a
        // resolved module domain reaches only `ItemModifier`. Charge nodes
        // are online and never active, so only the online effect or
        // passive/online-category effects run theirs.
        let sde = test_world.sde();
        let expected: std::collections::HashSet<i64> = sde.types.get(charge_type)
            .expect("picked charge type").effects().iter()
            .flat_map(|entry| sde.dogma.effect(entry.effect_id).into_iter()
                .filter(|effect| entry.effect_id == infinity_core::consts::dogma::EFFECT_ONLINE as i32
                    || matches!(effect.effect_category.unwrap_or(0), 0 | 4))
                .flat_map(|effect| effect.modifiers().iter()))
            .filter(|modifier| modifier.domain == "otherID")
            .map(|modifier| i64::from(modifier.modified))
            .collect();
        let (event, args) = rx.try_recv().expect("fitting delta");
        assert_eq!(event, "OnModuleAttributeChanges");
        let rows = args[0].items().unwrap();
        assert_eq!(rows.len(), expected.len());
        let mut actual = std::collections::HashSet::new();
        for row in rows {
            let fields = row.items().unwrap();
            assert_eq!(fields[2], Value::Int(module_id));
            actual.insert(fields[3].as_int().expect("fitting attribute"));
            assert_ne!(fields[5], fields[6], "a fitting row must carry a real change");
        }
        assert_eq!(actual, expected);
        let (event, args) = rx.try_recv().expect("final quantity delta");
        assert_eq!(event, "OnModuleAttributeChanges");
        let rows = args[0].items().unwrap();
        assert_eq!(rows.len(), 1);
        let fields = rows[0].items().unwrap();
        assert_eq!(fields[2], crate::services::combat::targeting_charges::charge_key(ship, flag, charge_type));
        assert_eq!(fields[3].as_int(), Some(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY)));
        assert_eq!(fields[5].as_int(), Some(0));
        assert_eq!(fields[6].as_int(), Some(1));
        assert!(rx.try_recv().is_err(), "final consumption must not prime or publish fitting");
        assert!(test_world.with(system, move |s| s.loaded_empty_proof(ship, module_id)).await.unwrap(), "an emptied slot proves the ancillary empty path");
    }).await;
    world.remove_ball(system, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(fixture.ship);
    crate::services::tests::fixture::cleanup_ship(&db, None, fixture).await.unwrap();
    result.unwrap();
}
