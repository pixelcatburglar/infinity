use super::*;
use crate::services::effects::tests::fixture;
use crate::services::effects::{lifecycle::{self, ActivationOwner, Module}, registry};
use crate::services::combat::{self, ActiveModule};
use crate::services::fit::Fit;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST,
    dogma::{ATTR_WARP_SCRAMBLE_STATUS, EFFECT_MICRO_JUMP_PORTAL}};
use infinity_core::messages;
use crate::space::{Ball, MODE_GOTO};

/// Fixture: client dogma/const.py attributeDisallowInHighSec.
const DISALLOW_IN_HIGH_SEC: i32 = 1970;

#[test]
fn select_passengers_keeps_everyone_under_cap_in_id_order() {
    let input = vec![3, 1, 2];
    assert_eq!(select_passengers(input.clone(), 3, || panic!("no draw under cap")), input);
    assert_eq!(select_passengers(input.clone(), 4, || panic!("no draw under cap")), input);
    assert_eq!(select_passengers(vec![1, 2, 3], 0, || panic!("no draw at cap zero")), Vec::<i32>::new());
}

#[test]
fn select_passengers_is_deterministic_for_an_injected_sequence() {
    let mut draws = [0.6, 0.25].into_iter();
    let picked = select_passengers(vec![1, 2, 3, 4, 5], 2, || draws.next().unwrap());
    assert_eq!(picked, [4, 3]);
    let mut unique = picked.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), picked.len());
}

#[test]
fn hull_eligibility_excludes_capital_freighter_and_industrial_command_groups() {
    assert!(hull_eligible(Some(CATEGORY_SHIP), Some(infinity_core::consts::GROUP_FRIGATE)));
    assert!(hull_eligible(Some(CATEGORY_SHIP), Some(infinity_core::consts::GROUP_CAPSULE)));
    for group in EXCLUDED_HULL_GROUPS {
        assert!(!hull_eligible(Some(CATEGORY_SHIP), Some(group)), "group {group}");
    }
    assert!(!hull_eligible(Some(CATEGORY_MODULE), Some(infinity_core::consts::GROUP_FRIGATE)));
    assert!(!hull_eligible(Some(CATEGORY_SHIP), None));
    assert!(!hull_eligible(None, None));
}

/// A steadily cruising fixture ball: GOTO at full speed straight along -Z;
/// identical balls integrate identically, so one serves as the other's control.
fn cruising(mut ball: Ball) -> Ball {
    ball.free = true;
    ball.mode = MODE_GOTO;
    ball.goto = [ball.pos[0], 0.0, -1e9];
    ball.max_vel = 10.0;
    ball.speed_fraction = 1.0;
    ball.vel = [0.0, 0.0, -10.0];
    ball.old_vel = [0.0, 0.0, -10.0];
    ball
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn field_jump_moves_source_and_eligible_passengers_by_one_displacement() {
    use crate::services::tests::fixture::{create_ship, cleanup_ship, ShipItemSpec, ShipLocation};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && hull_eligible(t.category_id, t.group_id)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICRO_JUMP_PORTAL)).unwrap().type_id;
    let effect = registry::resolve(&sde, EFFECT_MICRO_JUMP_PORTAL).unwrap();
    let module_spec = ShipItemSpec { type_id: module_type, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] };
    let source = create_ship(&db, hull, ShipLocation::Space { system: 1 }, &[module_spec]).await.unwrap();
    let eligible = create_ship(&db, hull, ShipLocation::Space { system: 1 }, &[]).await.unwrap();
    let scrammed = create_ship(&db, hull, ShipLocation::Space { system: 1 }, &[]).await.unwrap();
    let distant = create_ship(&db, hull, ShipLocation::Space { system: 1 }, &[]).await.unwrap();
    crate::dao::items::dogma::set_override(&db, scrammed.ship, ATTR_WARP_SCRAMBLE_STATUS, 1.0).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let module = source.module();
    let outcome = crate::tests::catch_scenario({
        let db = db.clone();
        let world = world.clone();
        let effect = effect.clone();
        let (source_id, eligible_id, scrammed_id, distant_id) = (source.ship, eligible.ship, scrammed.ship, distant.ship);
        let characters = (source.character, eligible.character, scrammed.character, distant.character);
        async move {
            fixture::admit(&db, &world, 1, fixture::ball(source_id, hull, characters.0, Some(characters.0), 0.0)).await;
            let carried = cruising(fixture::ball(eligible_id, hull, characters.1, Some(characters.1), 2_000.0));
            fixture::admit(&db, &world, 1, carried).await;
            fixture::admit(&db, &world, 1, fixture::ball(scrammed_id, hull, characters.2, Some(characters.2), 2_500.0)).await;
            let control = cruising(fixture::ball(distant_id, hull, characters.3, Some(characters.3), 20_000.0));
            fixture::admit(&db, &world, 1, control).await;
            // Admissions can cross a tick; compare from when both balls are present.
            let separation_before = world.with(1, move |s| {
                crate::space::sub(s.ball(eligible_id).unwrap().pos, s.ball(distant_id).unwrap().pos)
            }).await.unwrap();
            let (notify, _) = tokio::sync::mpsc::unbounded_channel();
            let cx = Ctx { db: db.clone(), world: world.clone(), notify, char_id: characters.0, ship_id: source_id, system_id: 1 };
            let generation = Arc::new(AtomicBool::new(false));
            {
                let mut st = combat::state();
                st.active.insert(module, ActiveModule { char_id: characters.0, stop: generation.clone(), forced_inactive: false });
                st.activation_generation.insert(module, generation.clone());
            }
            let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }],
                presented_effect: effect.id, effect, target: None, repeat: 1, stop: generation.clone(),
                owner: ActivationOwner { master: module, generation: generation.clone(), armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
                started: 0, charge_type: None, cycles: 0, duration_ms: 1.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
                source_membership: None, target_type: None, inputs: crate::services::effects::parameters::CycleInputs::default() };
            cycle = fixture::capture_cycle(cycle).await;
            assert_eq!(end(&cx, &mut cycle).await.ok().unwrap(),
                CycleOutcome::Ended { reason: "micro jump field completed", outcome: ModuleCycleOutcome::Completed });
            assert!(world.after(1, 1200).await);
            assert_eq!(world.ball(1, source_id).await.unwrap().pos, [0.0, 0.0, -100_000.0]);
            let (carried_after, control_after) = world.with(1, move |s| {
                (s.ball(eligible_id).cloned(), s.ball(distant_id).cloned())
            }).await.unwrap();
            let (carried_after, control_after) = (carried_after.unwrap(), control_after.unwrap());
            // Same actor turn and identical -Z cruise: the position delta
            // carries only the jump's shared displacement; the control keeps
            // travelling and the carried ball preserves the nonzero velocity.
            assert_eq!([carried_after.pos[0] - control_after.pos[0], carried_after.pos[1] - control_after.pos[1], carried_after.pos[2] - control_after.pos[2]],
                [separation_before[0], separation_before[1], separation_before[2] - 100_000.0]);
            assert_eq!(carried_after.vel, control_after.vel);
            assert!(control_after.vel[2] < 0.0);
            assert!(control_after.pos[2] < 0.0);
            assert_eq!(world.ball(1, scrammed_id).await.unwrap().pos, [2_500.0, 0.0, 0.0]);
        }
    }).await;
    {
        let mut st = combat::state();
        st.active.remove(&module);
        st.activation_generation.remove(&module);
    }
    for ship_fixture in [source, eligible, scrammed, distant] {
        cleanup_ship(&db, Some(&*world), ship_fixture).await.unwrap();
    }
    outcome.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn security_class_check_refuses_disallowed_module_in_highsec_only() {
    use crate::services::tests::fixture::{create_ship, cleanup_ship, ShipItemSpec, ShipLocation};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICRO_JUMP_PORTAL)).unwrap().type_id;
    let module_spec = ShipItemSpec { type_id: module_type, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] };
    let ship_fixture = create_ship(&db, hull, ShipLocation::Space { system: 1 }, &[module_spec]).await.unwrap();
    let module = ship_fixture.module();
    let (ship_id, character) = (ship_fixture.ship, ship_fixture.character);
    let highsec = sde.map.systems()
        .find(|s| s.security.is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold))
        .unwrap().solar_system_id;
    let outcome = crate::tests::catch_scenario({
        let db = db.clone();
        let sde = sde.clone();
        async move {
            let fit = Fit::load(&db, sde.clone(), character, ship_id).await.unwrap();
            let Err(CallError::User { msg, dict }) = lifecycle::security_class_check(&sde, &fit, module, highsec)
                else { panic!("disallowed module admitted in high-sec") };
            assert_eq!(msg, messages::MSG_CANT_IN_HIGH_SEC_SPACE);
            assert!(dict.is_empty());
            assert!(lifecycle::security_class_check(&sde, &fit, module, 1).is_ok());
            crate::dao::items::dogma::set_override(&db, module, DISALLOW_IN_HIGH_SEC, 0.0).await.unwrap();
            let fit = Fit::load(&db, sde.clone(), character, ship_id).await.unwrap();
            assert!(lifecycle::security_class_check(&sde, &fit, module, highsec).is_ok());
        }
    }).await;
    cleanup_ship(&db, None, ship_fixture).await.unwrap();
    outcome.unwrap();
}
