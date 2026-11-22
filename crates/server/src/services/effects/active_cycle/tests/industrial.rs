//! Industrial and compact cores share the tracked mode lifecycle. Discovery,
//! fuel, drone propagation and compression dependence use the loaded SDE and
//! the parent in-space fixture. No type list is copied into the test.
use super::*;
use crate::services::effects::{compression, lifecycle::{ActivationOwner, Cycle, Module}, parameters::MasterPacket};
use crate::services::fit::{Fit, Key};
use crate::services::skills::gm::{self, Change};
use crate::services::tests::fixture::ShipItemSpec;
use infinity_core::consts::{CATEGORY_DRONE, FLAG_CARGO, FLAG_DRONE_BAY, GROUP_SOLAR_SYSTEM};
use infinity_core::consts::dogma::{ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE, ATTR_CONSUMPTION_QUANTITY, ATTR_CONSUMPTION_TYPE, ATTR_DRONE_BANDWIDTH, ATTR_DRONE_BANDWIDTH_USED, ATTR_DRONE_CAPACITY, ATTR_MAX_ACTIVE_DRONES, ATTR_MAX_RANGE, EFFECT_INDUSTRIAL_COMPACT_CORE, EFFECT_INDUSTRIAL_CORE, EFFECT_INDUSTRIAL_ITEM_COMPRESSION};
use std::sync::atomic::AtomicBool;

fn raw_units(sde: &Sde, module_type: i32) -> i64 {
    sde.types.get(module_type).and_then(|data| data.attribute(ATTR_CONSUMPTION_QUANTITY)).unwrap_or(0.0).ceil().max(1.0) as i64
}

fn fuel_type(sde: &Sde, module_type: i32) -> i32 {
    sde.types.get(module_type).expect("core type").attribute(ATTR_CONSUMPTION_TYPE).expect("fuel type") as i32
}

/// The loaded owner-skill row the evaluator applies to `Key::Drone`.
fn drone_owner_modifier(sde: &Sde, effect: i32) -> Option<(i32, i32, i32)> {
    sde.dogma.effect(effect)?.modifiers().iter().find(|row| row.func == "OwnerRequiredSkillModifier" && row.domain == "charID" && row.skill.is_some() && row.modifying != row.modified)
        .map(|row| (row.modified, row.modifying, row.skill.expect("skill")))
}

fn skill_matched_drone(sde: &Sde, skill: i32) -> i32 {
    sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_DRONE)
        && ty.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|used| used > 0.0)
        && crate::services::skills::requirements::requirements(sde, ty.type_id).any(|(required, _)| required == skill))
        .map(|ty| ty.type_id).expect("published drone requiring the modifier skill")
}


fn requiring_compressor(sde: &Sde, slot: i32) -> ShipItemSpec<'static> {
    let (type_id, flag) = sde.types.iter().find_map(|ty| {
        if !(ty.published == Some(true) && ty.effects().iter().any(|effect| effect.effect_id == EFFECT_INDUSTRIAL_ITEM_COMPRESSION)
            && ty.attribute(ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE).is_some_and(|value| value > 0.0)) { return None; }
        Some((ty.type_id, crate::services::effects::fitting::rack(&sde.types, ty.type_id)?.2 + slot))
    }).expect("published compressor with a positive core requirement");
    ShipItemSpec { type_id, flag_id: flag, quantity: 1, singleton: true, online: true, attributes: &[] }
}

async fn resident_drone(space: &InSpace, drone: i64, attribute: i32) -> f64 {
    let system = space.system;
    space.client.world.with(system, move |s| s.with_combat_fit(drone, |fit, key, _| {
        assert_eq!(key, Key::Drone(drone), "the launched drone is the resident record");
        fit.value(key, attribute)
    })).await.unwrap().expect("resident drone value")
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn loaded_sde_links_eight_published_industrial_and_compact_modules() {
    scenario_spec(EFFECT_INDUSTRIAL_CORE, &[], &[], async |_space, sde, _db| {
        let industrial = published_links(sde, EFFECT_INDUSTRIAL_CORE);
        let compact = published_links(sde, EFFECT_INDUSTRIAL_COMPACT_CORE);
        let mut all = industrial.clone();
        all.extend(&compact);
        all.sort_unstable();
        let count = all.len();
        all.dedup();
        assert_eq!(count, 8, "both families publish eight linked modules");
        assert_eq!(all.len(), 8, "the eight links name distinct published types");
    }).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn each_core_consumes_two_prepared_requirements_then_refuses() {
    for effect in [EFFECT_INDUSTRIAL_CORE, EFFECT_INDUSTRIAL_COMPACT_CORE] {
        crate::tests::init_config();
        let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = crate::tests::shared_sde(&db).await;
        let module_type = module_type(&sde, effect);
        let fuel = ShipItemSpec { type_id: fuel_type(&sde, module_type), flag_id: FLAG_CARGO, quantity: raw_units(&sde, module_type) * 2, singleton: false, online: false, attributes: &[] };
        scenario_spec(effect, &[], &[fuel], async |space, sde, db| {
            let (system, module, ship) = (space.system, space.module, space.fixture.ship);
            let stack = space.fixture.items[1];
            let units = effective_units(space, module).await;
            assert!(units > 0, "resident fit requires positive fuel");
            db.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&stack, &(units * 2)]).await.unwrap();
            assert_eq!(quantity(db, stack).await, units * 2, "two effective requirements are prepared before activation");
            let name = effect_name(sde, effect);
            activate(space, &name, module).await;
            until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
            assert_eq!(quantity(db, stack).await, units, "the first cycle consumes one effective requirement");
            let started = match combat::state().active_cycles.get(&module) { Some(ActiveCycle::Running { start, generation, .. }) => (*start, generation.clone()), _ => panic!("running") };
            until(&space.client.world, system, 8_000, || now_ms() >= started.0 + DURATION_MS as i64 + 400).await;
            assert_eq!(quantity(db, stack).await, 0, "the second cycle consumes the prepared fuel");
            until(&space.client.world, system, 8_000, || record(module) == Record::None).await;
            let _ = drain(&mut space.rx);
            assert_eq!(effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(module), Value::str(&name), Value::None, Value::Int(100)]).await.ok(), Some(Value::None), "the shared activation queues before fuel is consumed");
            let mut notices = Vec::new();
            until(&space.client.world, system, 8_000, || { notices.extend(drain(&mut space.rx)); notices.iter().any(|(event, args)| event == "OnEffectMessage" && *args == vec![Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY), Value::Dict(vec![(Value::str("notify"), Value::unicode(infinity_core::messages::effects::INSUFFICIENT_FUEL))])]) && record(module) == Record::None && !combat::state().active.contains_key(&module) }).await;
            assert!(notices.iter().all(|(event, args)| !(event == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(module)) && args.get(3) == Some(&Value::Int(1)))), "the refused start publishes no Godma start");
            assert!(combat::state().active.get(&module).is_none(), "the refused generation leaves no activation");
            assert_eq!(space.client.world.with(system, move |s| s.resident_fit(ship, |fit, _| fit.module_state(module))).await.unwrap(), Some(Some((true, false))), "the module stays online and inactive");
            assert_eq!(quantity(db, stack).await, 0, "the refusal consumes no fuel");
            assert_eq!(record(module), Record::None, "the refusal leaves the mode inactive");
        }).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn a_launched_drone_and_one_real_compressor_follow_the_core() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (modified, operand, skill) = drone_owner_modifier(&sde, EFFECT_INDUSTRIAL_CORE).expect("industrial owner-skill drone modifier");
    let primary = module_type(&sde, EFFECT_INDUSTRIAL_CORE);
    let compressor = requiring_compressor(&sde, 1);
    let pending_compressor = requiring_compressor(&sde, 2);
    let drone = skill_matched_drone(&sde, skill);
    let fuel = ShipItemSpec { type_id: fuel_type(&sde, primary), flag_id: FLAG_CARGO, quantity: raw_units(&sde, primary) * 4, singleton: false, online: false, attributes: &[] };
    let specs = [compressor, pending_compressor, ShipItemSpec { type_id: drone, flag_id: FLAG_DRONE_BAY, quantity: 1, singleton: true, online: false, attributes: &[] }, fuel];
    scenario_spec(EFFECT_INDUSTRIAL_CORE, &[(operand, 25.0)], &specs, async |space, sde, db| {
        let (system, core, ship, character) = (space.system, space.module, space.fixture.ship, space.fixture.character);
        let (compressor_id, pending_module, drone_id) = (space.fixture.items[1], space.fixture.items[2], space.fixture.items[3]);
        let duration = sde.dogma.effect(EFFECT_INDUSTRIAL_ITEM_COMPRESSION).and_then(|effect| effect.duration_attribute_id).expect("compressor duration attribute");
        for (item, attr, value) in [(ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0), (ship, ATTR_DRONE_CAPACITY, 1_000_000.0), (ship, ATTR_MAX_ACTIVE_DRONES, 5.0), (compressor_id, ATTR_MAX_RANGE, 20_000.0), (pending_module, ATTR_MAX_RANGE, 20_000.0), (compressor_id, duration, DURATION_MS)] {
            item_dogma::set_override(db, item, attr, value).await.unwrap();
        }
        effects_fixture::publish_fit(db, &space.client.world, system, character, ship).await;
        gm::apply(&mut space.client, character, Change::Set(vec![(skill, 5)])).await.expect("modifier skill");
        let effect = crate::services::effects::registry::resolve(sde, EFFECT_INDUSTRIAL_ITEM_COMPRESSION).expect("compression");
        let cx = Ctx { db: db.clone(), world: space.client.world.clone(), notify: space.client.notify_tx.clone(), char_id: character, ship_id: ship, system_id: system };
        let cold = Fit::load_in_space(db, &space.client.world, system, character, ship).await.unwrap();
        assert!(compression::validate(&cx, &cold, &Module { id: compressor_id, type_id: compressor.type_id, flag: compressor.flag_id }, &effect, None).await.is_err(), "no live core refuses the compressor");
        let kwargs = Value::Dict(vec![]);
        let launched = crate::services::drones::inventory::launch::launch(&mut space.client, &crate::services::Call { service: "ship", method: "LaunchDrones",
            args: &[Value::list([Value::tuple([Value::Int(drone_id), Value::Int(1)])]), Value::None, Value::Bool(false)], kwargs: &kwargs, bound: true,
            bind_params: Some(&Value::list([Value::Int(system), Value::Int(GROUP_SOLAR_SYSTEM)])) }).await.expect("launch");
        assert_eq!(launched, Value::Dict(vec![(Value::Int(drone_id), Value::list([Value::Int(drone_id)]))]), "the skill-matched drone launches before the core");
        let before = resident_drone(space, drone_id, modified).await;
        let name = effect_name(sde, EFFECT_INDUSTRIAL_CORE);
        activate(space, &name, core).await;
        until(&space.client.world, system, 5_000, || matches!(record(core), Record::Running)).await;
        assert_ne!(resident_drone(space, drone_id, modified).await, before, "the resident drone changes through the owner-skill modifier");
        let live = Fit::load_in_space(db, &space.client.world, system, character, ship).await.unwrap();
        compression::validate(&cx, &live, &Module { id: compressor_id, type_id: compressor.type_id, flag: compressor.flag_id }, &effect, None).await.expect("the live core admits compression");
        let compressor_name = effect_name(sde, EFFECT_INDUSTRIAL_ITEM_COMPRESSION);
        effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(compressor_id), Value::str(&compressor_name), Value::None, Value::Int(100)]).await.expect("real compressor");
        let advertised = |ball: &crate::space::Ball| ball.slim.iter().any(|(key, _)| *key == "compression_facility_typelists");
        let mut admitted = false;
        for _ in 0..80 {
            admitted = space.client.world.with(system, move |s| combat::state().active.contains_key(&compressor_id) && s.ball(ship).is_some_and(&advertised)).await.unwrap_or(false);
            if admitted { break; }
            assert!(space.client.world.after(system, 100).await, "system actor stopped");
        }
        assert!(admitted, "the owned compressor generation and facility are both visible");
        assert!(advertised(&space.client.world.ball(system, ship).await.unwrap()), "the admitted compressor is advertised");
        assert!(combat::state().active.contains_key(&compressor_id), "the owned compressor generation remains active");
        let pending = Arc::new(AtomicBool::new(false));
        combat::state().active.insert(pending_module, combat::ActiveModule { char_id: character, stop: pending.clone(), forced_inactive: false });
        let mut pending_cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: pending_module, type_id: pending_compressor.type_id, flag: pending_compressor.flag_id }], presented_effect: i64::from(EFFECT_INDUSTRIAL_ITEM_COMPRESSION), effect: effect.clone(), target: None, repeat: 1, stop: pending.clone(),
            owner: ActivationOwner { master: pending_module, generation: pending.clone(), armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1_000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false, source_membership: None, target_type: None,
            inputs: crate::services::effects::parameters::CycleInputs::default() };
        pending_cycle = effects_fixture::capture_cycle(pending_cycle).await;
        assert!(matches!(pending_cycle.inputs.frame.as_ref().map(|frame| &frame.packet), Some(MasterPacket::Compression(Ok(inputs))) if inputs.core > 0.0), "the second compressor captures a positive requirement while the core is live");
        item_dogma::set_override(db, pending_module, ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE, 0.0).await.unwrap();
        effects_fixture::publish_fit(db, &space.client.world, system, character, ship).await;
        let membership = space.client.world.with(system, move |s| s.combat_membership_generation(ship)).await.unwrap().expect("membership");
        let (original, remaining) = (combat::state().active.get(&core).expect("core").stop.clone(), Arc::new(AtomicBool::new(false)));
        let original_terminal = { let mut state = combat::state(); crate::services::effects::active_cycle::terminate(&mut state, core, &original).expect("original terminal") };
        space.client.world.with(system, { let remaining = remaining.clone(); move |s| {
            crate::services::tests::fixture::install_running_mode(core, ship, character, i64::from(EFFECT_INDUSTRIAL_CORE), membership, &remaining);
            { let mut state = combat::state(); state.active.insert(core, combat::ActiveModule { char_id: character, stop: remaining.clone(), forced_inactive: false }); state.activation_generation.insert(core, remaining.clone()); }
            crate::services::effects::active_cycle::apply_terminal(s, original_terminal);
            assert!(compression::live_core(s, ship), "the isolated replacement core remains live");
            assert!(advertised(s.ball(ship).expect("ship")), "the remaining core keeps the real facility");
        }}).await.unwrap();
        space.client.world.with(system, { let remaining = remaining.clone(); move |s| {
            let terminal = { let mut state = combat::state(); crate::services::effects::active_cycle::terminate(&mut state, core, &remaining).expect("replacement terminal") };
            crate::services::effects::active_cycle::apply_terminal(s, terminal);
        }}).await.unwrap();
        assert!(!advertised(&space.client.world.ball(system, ship).await.unwrap()), "removing the last isolated core withdraws the facility");
        until(&space.client.world, system, 8_000, || !combat::state().active.contains_key(&compressor_id)).await;
        assert!(combat::state().active.get(&compressor_id).is_none(), "the owned cycle completes the compressor retirement");
        assert_eq!(resident_drone(space, drone_id, modified).await, before, "retirement restores the resident drone value");
        assert!(combat::state().active.contains_key(&pending_module), "the unadvertised generation is still valid");
        assert_eq!(pending_cycle.source_membership, Some(membership), "the captured membership is the current one");
        assert!(matches!(compression::start(&cx, &mut pending_cycle).await.expect("pending start"), crate::services::effects::lifecycle::CycleOutcome::Stop(_)), "the captured generation fails its first start after the core retires");
        combat::state().active.remove(&pending_module);
        item_dogma::set_override(db, compressor_id, ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE, 0.0).await.unwrap();
        effects_fixture::publish_fit(db, &space.client.world, system, character, ship).await;
        let open = Fit::load_in_space(db, &space.client.world, system, character, ship).await.unwrap();
        compression::validate(&cx, &open, &Module { id: compressor_id, type_id: compressor.type_id, flag: compressor.flag_id }, &effect, None).await.expect("a zero requirement keeps prior admission");
    }).await;
}
