//! Bastion and Sacribis share the tracked record but keep the repeating
//! scheduler. Acceptance publishes one Godma start and one current effect,
//! and a stop request holds the fitted bonus until the cycle retires.
use super::*;
use crate::services::{combat::tests::remote_repair_db::set_locked_target, effects::{cloak, lifecycle::{self, ActivationOwner, Cycle, Module}, projection::Projection, EffectKind, JammingType}, CallError};
use crate::services::tests::fixture::ShipItemSpec;
use crate::space;
use infinity_core::consts::{CATEGORY_MODULE, FLAG_CARGO, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST};
use infinity_core::consts::dogma::{ATTR_DISALLOW_EARLY_DEACTIVATION, ATTR_SCAN_RESOLUTION, EFFECT_BASTION, EFFECT_CLOAKING, EFFECT_NPC_BEHAVIOR_DAMP, EFFECT_SACRIBIS_BASTION};
use std::sync::atomic::AtomicBool;


/// The mode effect's first ship-domain modifier, the value the resident fit changes.
fn ship_modifier(sde: &Sde, effect: i32) -> i32 {
    sde.dogma.effect(effect).expect("mode effect").modifiers().iter()
        .find(|modifier| modifier.domain == "shipID").map(|modifier| modifier.modified)
        .expect("mode ship modifier")
}

async fn activate_until_running(space: &mut InSpace, name: &str) {
    let args = activate_args(space, name);
    effects_fixture::dogma_call(&mut space.client, "Activate", &args).await.ok().expect("activate");
    until(&space.client.world, space.system, 5_000, || matches!(record(space.module), Record::Running)).await;
}

fn mode_refused(error: CallError) -> bool {
    let text = Value::unicode(infinity_core::messages::effects::MODE_ACTION_BLOCKED);
    matches!(error, CallError::User { msg, dict } if msg == messages::MSG_CUSTOM_NOTIFY && dict.iter().any(|(_, value)| value == &text))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn bastion_admits_once_and_keeps_the_bonus_until_retirement() {
    for effect in [EFFECT_BASTION, EFFECT_SACRIBIS_BASTION] {
        scenario_spec(effect, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], &[], async |space, sde, _db| {
            let links = sde.types.iter().filter(|ty| ty.published == Some(true) && ty.effects().iter().any(|linked| linked.effect_id == EFFECT_BASTION || linked.effect_id == EFFECT_SACRIBIS_BASTION)).count();
            assert_eq!(links, 6, "Bastion and Sacribis publish six module links");
            let (system, module, ship, character) = (space.system, space.module, space.fixture.ship, space.fixture.character);
            let name = effect_name(sde, effect);
            let attribute = ship_modifier(sde, effect);
            let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
            space.client.world.observe(system, character, ship, destiny).await.unwrap();
            let generation = Arc::new(AtomicBool::new(false));
            { let mut state = combat::state(); crate::services::effects::active_cycle::register(&mut state, module, ship, i64::from(effect), &generation); state.active.insert(module, combat::ActiveModule { char_id: character, stop: generation, forced_inactive: false }); }
            let pending = space.client.world.with(system, move |s| { let values = s.with_combat_fit(ship, |fit, _, _| (fit.value(crate::services::fit::Key::Ship, attribute), fit.value(crate::services::fit::Key::Ship, ATTR_SCAN_RESOLUTION))).expect("base"); (values.0, crate::services::effects::active_cycle::mode_locked(s, ship), values.1) }).await.unwrap();
            assert!(matches!(record(module), Record::Pending) && !pending.1, "controlled pending carries no lock");
            let base = pending.0;
            combat::state().active_cycles.remove(&module); combat::state().active.remove(&module);
            let damp = crate::services::effects::registry::resolve(sde, EFFECT_NPC_BEHAVIOR_DAMP).expect("damp");
            let resistance = damp.attribute(crate::services::effects::registry::EffectAttribute::Resistance).expect("damp resistance selector");
            let modifying = sde.dogma.effect(EFFECT_NPC_BEHAVIOR_DAMP).expect("damp").modifiers().iter().find(|modifier| modifier.domain == "targetID" && modifier.modified == ATTR_SCAN_RESOLUTION).expect("scan modifier").modifying;
            space.client.world.with(system, move |s| {
                let mut other = s.ball(ship).expect("ship").clone(); other.id = -ship; other.char_id = None; other.pos[0] += 1_000.0; other.old_pos = other.pos; s.insert(other.clone());
                s.upsert_projection(ship, Projection { source_ship: -ship, source_module: -11, source_type: other.type_id, effect_id: EFFECT_NPC_BEHAVIOR_DAMP, kind: EffectKind::Projected { jamming: JammingType::Damp }, until_ms: i64::MAX, values: vec![(modifying, 50.0)], charge_values: vec![], charge_type: None, resistance_attribute: Some(resistance), scramble: None });
            }).await.unwrap();
            space.client.world.with(system, move |s| { s.command(ship, |ball, _, _| { ball.mode = crate::space::MODE_WARP; Vec::new() }); crate::space::system::tests::events::publish_scene_prefix(s, false); }).await.unwrap();
            let warp_args = activate_args(space, &name);
            let Err(CallError::User { msg, .. }) = effects_fixture::dogma_call(&mut space.client, "Activate", &warp_args).await else { panic!("warp start") };
            assert_eq!(msg, messages::MSG_DENIED_ACTIVATE_IN_WARP);
            assert_eq!(record(module), Record::None, "a refused start leaves no record and no debit");
            space.client.world.with(system, move |s| { s.command(ship, move |ball, _, system| { assert!(ball.follow_ball(system.ball(-ship).expect("follow target"), 500.0)); Vec::new() }); crate::space::system::tests::events::publish_scene_prefix(s, false); }).await.unwrap();
            let boosted = space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_SCAN_RESOLUTION))).await.unwrap().unwrap();
            assert_ne!(boosted, pending.2, "pre-entry projection changes scan resolution");
            let _ = drain(&mut space.rx);
            activate_until_running(space, &name).await;
            assert!(active_effects(&mut space.client, module).await.contains(&i64::from(effect)));
            let values = space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| (fit.value(crate::services::fit::Key::Ship, attribute), fit.value(crate::services::fit::Key::Ship, ATTR_SCAN_RESOLUTION)))).await.unwrap().unwrap();
            let Err(CallError::User { msg, .. }) = module_online(&space.client, ship, module, false).await else { panic!("offline") };
            assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
            let Err(CallError::User { msg, .. }) = unfit(&mut space.client, module, ship).await else { panic!("unfit") };
            assert_eq!(msg, messages::MSG_CANNOT_REMOVE_ACTIVATED_MODULE);
            assert!(matches!(record(module), Record::Running), "refusals leave the admitted record");
            assert_ne!(values.0, base, "admitted mode changes the resident fit");
            assert!(if effect == EFFECT_BASTION { values.1 == boosted } else { pending.2 < values.1 && values.1 < boosted }, "ordinary Bastion preserves the damp; Sacribis partially reduces it");
            let membership = space.client.world.with(system, move |s| s.persistent_generation(ship)).await.unwrap().unwrap();
            let (jump_tx, jump_rx) = tokio::sync::oneshot::channel();
            let jump_generation = Arc::new(AtomicBool::new(false));
            space.client.world.with(system, move |s| { let pos = s.ball(ship).expect("ship").pos; crate::services::effects::mjd::queue_jump(s, ship, [pos[0] + 1_000.0, pos[1], pos[2]], jump_generation, membership, |_, _| true, move |ok| { let _ = jump_tx.send(ok); }); }).await.unwrap();
            assert!(space.client.world.after(system, 1_100).await);
            assert!(!jump_rx.await.unwrap_or(true), "live mode refuses the shared jump");
            let stopped = space.client.world.with(system, move |s| { let ball = s.ball(ship).expect("ship"); (ball.mode, ball.follow_id) }).await.unwrap();
            assert_eq!(stopped, (space::MODE_STOP, 0), "native Stop clears follow");
            let station = sde.map.stations().find(|row| row.solar_system_id == Some(system) && sde.map.celestial(row.station_id).is_some_and(|row| row.x.is_some())).expect("station").station_id;
            let celestial = sde.map.celestial(station).expect("celestial");
            let (station_pos, station_radius) = ([celestial.x.unwrap(), celestial.y.unwrap(), celestial.z.unwrap()], sde.celestial_radius(celestial));
            let dest = sde.map.systems().find(|row| row.solar_system_id != system && row.constellation_id.is_some() && row.region_id.is_some()).expect("destination").solar_system_id;
            space.client.session.apply(vec![("charid", Value::Int(character))]);
            space.client.world.with(system, move |s| { s.command(ship, move |ball, _, _| { ball.pos = station_pos; Vec::new() }); crate::space::system::tests::events::publish_scene_prefix(s, false); let mut station_body = s.ball(ship).expect("ship").clone(); station_body.id = station; station_body.char_id = None; station_body.radius = station_radius; s.insert(station_body); }).await.unwrap();
            let identity = crate::services::identity::Identity::of_client(&space.client);
            let docked = crate::services::ship::dock(&identity, &space.client.db, &space.client.world, Some(system), station, None::<&mut crate::client::Client>).await;
            assert!(mode_refused(match docked { Err(error) => error, Ok(_) => panic!("dock accepted") }), "live mode refuses dock");
            let jump = crate::services::ship::Jump { through: station, guid: "effects.JumpOut", other_type: None, dest_system: dest, pos: [0.0; 3] };
            let jumped = crate::services::ship::jump_out_and_arrive(&mut space.client, system, ship, character, jump).await;
            assert!(mode_refused(match jumped { Err(error) => error, Ok(_) => panic!("jump accepted") }), "live mode refuses jump");
            assert_eq!(fx_count(&mut events, module, 1), 1, "one current effect start");
            let stop_args = [Value::Int(module), Value::str(&name)];
            effects_fixture::dogma_call(&mut space.client, "Deactivate", &stop_args).await.ok().unwrap();
            until(&space.client.world, system, 5_000, || matches!(record(module), Record::None | Record::Cooling { .. })).await;
            let restored = space.client.world.with(system, move |s| { let follow = s.ball(ship).expect("ship").follow_id; (s.with_combat_fit(ship, |fit, _, _| (fit.value(crate::services::fit::Key::Ship, attribute), fit.value(crate::services::fit::Key::Ship, ATTR_SCAN_RESOLUTION))), follow) }).await.unwrap();
            assert_eq!(restored.0, Some((base, boosted)), "retirement restores the fit and the projection");
            assert_ne!(restored.1, -ship, "retirement does not resume the old follow");
            let notices = drain(&mut space.rx);
            assert_eq!(godma(&notices, module, i64::from(effect), 1), 1);
            assert_eq!(godma(&notices, module, i64::from(effect), 0), 1);
            assert_eq!(fx_count(&mut events, module, 0), 1, "one current effect stop");
        }).await;
    }
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn sacribis_repeats_without_a_second_start() {
    scenario_spec(EFFECT_SACRIBIS_BASTION, &[], &[], async |space, sde, _db| {
        let (system, module) = (space.system, space.module);
        let name = effect_name(sde, EFFECT_SACRIBIS_BASTION);
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        space.client.world.observe(system, space.fixture.character, space.fixture.ship, destiny).await.unwrap();
        let _ = drain(&mut space.rx);
        let repeat_args = activate_args(space, &name);
        effects_fixture::dogma_call(&mut space.client, "Activate", &repeat_args).await.ok().expect("activate");
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
        let started = match combat::state().active_cycles.get(&module) {
            Some(ActiveCycle::Running { generation, start, .. }) => (generation.clone(), *start),
            _ => panic!("running"),
        };
        assert_eq!(fx_count(&mut events, module, 1), 1, "one start");
        until(&space.client.world, system, 5_000, || now_ms() >= started.1 + DURATION_MS as i64 + 400).await;
        assert!(matches!(combat::state().active_cycles.get(&module), Some(ActiveCycle::Running { generation, .. }) if Arc::ptr_eq(generation, &started.0)), "repeat keeps the accepted generation");
        assert_eq!(godma(&drain(&mut space.rx), module, i64::from(EFFECT_SACRIBIS_BASTION), 1), 1, "repeat emits no second Godma start");
        space.client.world.unobserve(space.fixture.character);
    }).await;
}
fn published(sde: &Sde, kind: EffectKind, flag: i32) -> ShipItemSpec<'static> {
    let type_id = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
        && crate::services::effects::fitting::rack(&sde.types, ty.type_id).is_some_and(|(_, _, first)| first == flag)
        && ty.effects().iter().any(|effect| crate::services::effects::registry::resolve(sde, effect.effect_id).is_some_and(|resolved| resolved.kind == kind && resolved.manually_activatable())))
        .map(|ty| ty.type_id).expect("published module");
    ShipItemSpec { type_id, flag_id: flag, quantity: 1, singleton: true, online: true, attributes: &[] }
}
#[tokio::test] #[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn running_mode_allows_a_weapon_and_an_unrelated_offline() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (turret_type, charge_type) = crate::services::combat::targeting_charges::tests::compatible_ammunition(&sde);
    let effect = crate::services::effects::registry::resolve(&sde, sde.types.get(turret_type).expect("gun").effects().iter().find(|effect| crate::services::effects::registry::resolve(&sde, effect.effect_id).is_some_and(|resolved| resolved.kind == EffectKind::Turret)).expect("turret").effect_id).expect("resolved");
    let flag = crate::services::effects::fitting::rack(&sde.types, turret_type).expect("turret rack").2 + 1;
    let weapon = ShipItemSpec { type_id: turret_type, flag_id: flag, quantity: 1, singleton: true, online: true, attributes: &[] };
    let cargo = ShipItemSpec { type_id: charge_type, flag_id: FLAG_CARGO, quantity: 1, singleton: false, online: false, attributes: &[] };
    let other = published(&sde, EffectKind::Modifiers, FLAG_MED_SLOT_FIRST);
    scenario_spec(EFFECT_BASTION, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], &[weapon, other, cargo], async |space, sde, db| {
        let (system, module, ship) = (space.system, space.module, space.fixture.ship);
        let (gun, unrelated, charge) = (space.fixture.items[1], space.fixture.items[2], space.fixture.items[3]);
        crate::services::combat::targeting_charges::load_ammo_core(&mut space.client, ship, vec![gun], vec![charge], Some(ship)).await.expect("loaded turret charge");
        until(&space.client.world, system, 5_000, || !combat::state().reloading.contains_key(&gun)).await;
        let loaded = space.client.world.with(system, move |s| s.loaded_effective(ship, gun)).await.unwrap().expect("loaded charge");
        assert!(loaded.1 == charge_type && loaded.2 > 0 && !loaded.3, "loaded turret is ready");
        let mode_name = effect_name(sde, EFFECT_BASTION);
        let mode_args = activate_args(space, &mode_name);
        effects_fixture::dogma_call(&mut space.client, "Activate", &mode_args).await.ok().unwrap();
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
        let (hull, pos) = space.client.world.with(system, move |s| {
            let ball = s.ball(ship).expect("ship");
            let mut pos = ball.pos;
            pos[0] += f64::from(ball.radius) * 2.0;
            (ball.type_id, pos)
        }).await.unwrap();
        let target_fixture = crate::services::tests::fixture::create_ship(db, hull, crate::services::tests::fixture::ShipLocation::Space { system }, &[]).await.unwrap();
        let target = target_fixture.ship;
        let result = crate::tests::catch_scenario(async {
            let mut target_ball = effects_fixture::ball(target, hull, target_fixture.character, None, pos[0]); target_ball.pos = pos; target_ball.old_pos = pos;
            effects_fixture::admit(db, &space.client.world, system, target_ball).await;
            set_locked_target(space.fixture.character, target, true);
            let gun_name = effect_name(sde, effect.id as i32);
            let gun_args = [Value::Int(gun), Value::str(&gun_name), Value::Int(target), Value::Int(100)];
            let before = crate::services::combat::tests::remote_repair_db::condition(&space.client.world, system, target).await;
            assert!(effects_fixture::dogma_call(&mut space.client, "Activate", &gun_args).await.is_ok(), "activation path admits the loaded turret");
            assert!(space.client.world.after(system, 5_000).await);
            assert!(crate::services::combat::tests::remote_repair_db::condition(&space.client.world, system, target).await.now.iter().sum::<f64>() < before.now.iter().sum::<f64>(), "the admitted turret consumes into target condition");
            assert!(module_online(&space.client, ship, unrelated, false).await.is_ok(), "unrelated module offlines");
            assert!(module_online(&space.client, ship, module, false).await.is_err(), "mode subject stays locked");
        }).await;
        set_locked_target(space.fixture.character, target, false);
        crate::services::tests::fixture::cleanup_ship(db, Some(&space.client.world), target_fixture).await.unwrap();
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }).await;
}
#[tokio::test] #[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn admitted_mode_refuses_a_queued_cloak_and_a_preexisting_cloak_refuses_the_mode() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let cloak_spec = published(&sde, EffectKind::Cloak, FLAG_HI_SLOT_FIRST);
    scenario_spec(EFFECT_BASTION, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], &[cloak_spec], async |space, sde, db| {
        let (system, module, ship, cloak_item) = (space.system, space.module, space.fixture.ship, space.fixture.items[1]);
        let cloak_generation = Arc::new(AtomicBool::new(false));
        combat::state().active.insert(cloak_item, combat::ActiveModule { char_id: space.fixture.character, stop: cloak_generation.clone(), forced_inactive: false });
        assert!(space.client.world.with(system, { let generation = cloak_generation.clone(); move |s| s.begin_module_cloak(ship, cloak_item, generation, 1_000, 1_000, |_| true, None) }).await.unwrap());
        assert!(space.client.world.after(system, 1_100).await);
        let cloaked_name = effect_name(sde, EFFECT_BASTION);
        let cloaked_args = activate_args(space, &cloaked_name);
        assert!(effects_fixture::dogma_call(&mut space.client, "Activate", &cloaked_args).await.is_err(), "preexisting module cloak refuses the mode");
        assert_eq!(record(module), Record::None);
        assert_eq!(space.client.world.with(system, move |s| s.ball(ship).map(|ball| ball.cloak)).await.unwrap(), Some(1), "refusal does not uncloak first");
        space.client.world.with(system, { let generation = cloak_generation.clone(); move |s| s.stop_module_cloak(ship, cloak_item, &generation) }).await.unwrap();
        assert!(space.client.world.after(system, 1_100).await);
        combat::state().active.remove(&cloak_item);
        let admitted_name = effect_name(sde, EFFECT_BASTION);
        activate_until_running(space, &admitted_name).await;
        let effect = crate::services::effects::registry::resolve(sde, EFFECT_CLOAKING).expect("cloak");
        let generation = Arc::new(AtomicBool::new(false));
        combat::state().active.insert(cloak_item, combat::ActiveModule { char_id: space.fixture.character, stop: generation.clone(), forced_inactive: false });
        let cx = Ctx { db: db.clone(), world: space.client.world.clone(), notify: space.client.notify_tx.clone(), char_id: space.fixture.character, ship_id: ship, system_id: system };
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: cloak_item, type_id: cloak_spec.type_id, flag: FLAG_HI_SLOT_FIRST }], presented_effect: effect.id, effect, target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: cloak_item, generation: generation.clone(), armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system }, started: 0, charge_type: None, cycles: 0, duration_ms: 1_000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false, source_membership: None, target_type: None, inputs: crate::services::effects::parameters::CycleInputs::default() };
        cycle = effects_fixture::capture_cycle(cycle).await;
        assert!(matches!(cloak::start(&cx, &mut cycle).await.unwrap(), lifecycle::CycleOutcome::Stop(_)), "admitted mode refuses the queued cloak");
        assert!(!space.client.world.with(system, move |s| s.has_module_cloak(ship)).await.unwrap());
        combat::state().active.remove(&cloak_item);
    }).await;
}
#[tokio::test] #[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn a_delayed_terminal_retires_only_the_original_generation() {
    scenario_spec(EFFECT_BASTION, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], &[], async |space, sde, _db| {
        let (system, module, ship) = (space.system, space.module, space.fixture.ship);
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        space.client.world.observe(system, space.fixture.character, ship, destiny).await.unwrap();
        let successor_name = effect_name(sde, EFFECT_BASTION);
        let successor_args = activate_args(space, &successor_name);
        effects_fixture::dogma_call(&mut space.client, "Activate", &successor_args).await.ok().unwrap();
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
        let mut active = combat::state().active.remove(&module).expect("active");
        let (original, owner) = (active.stop.clone(), active.char_id);
        let terminal = { let mut state = combat::state(); crate::services::effects::active_cycle::terminate(&mut state, module, &original).expect("consumed original") };
        let successor = Arc::new(AtomicBool::new(false));
        let membership = space.client.world.with(system, move |s| s.persistent_generation(ship)).await.unwrap().unwrap();
        active.stop = successor.clone();
        { let mut state = combat::state(); state.active.insert(module, active); state.activation_generation.insert(module, successor.clone()); }
        let accepted = space.client.world.with(system, { let successor = successor.clone(); move |s| {
            crate::services::tests::fixture::install_running_mode(module, ship, owner, i64::from(EFFECT_BASTION), membership, &successor);
            s.fitted_module_active(ship, module, true);
            crate::services::effects::active_cycle::apply_terminal(s, terminal);
            s.with_combat_fit(ship, |fit, _, _| fit.module_state(module))
        }}).await.unwrap();
        assert_eq!(accepted.flatten(), Some((true, true)), "successor fitted bit survives the old terminal");
        assert!(matches!(record(module), Record::Running), "successor running record survives");
        assert_eq!(fx_count(&mut events, module, 0), 1, "original FX retires once");
        assert_eq!(godma(&drain(&mut space.rx), module, i64::from(EFFECT_BASTION), 0), 0, "old terminal emits no successor Godma stop");
    }).await;
}

