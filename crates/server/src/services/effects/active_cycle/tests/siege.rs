//! Published siege modules share the tracked repeating mode. The four linked
//! types, their fuel and the XL turret and capital local repair they modify
//! come from the loaded SDE and the resident fit. No type catalogue is copied.
use super::*;
use crate::dao::items::dogma as item_dogma;
use crate::services::combat::tests::remote_repair_db::{condition, set_locked_target};
use crate::services::effects::lifecycle::{ActivationOwner, Cycle, Module};
use crate::services::effects::{registry::kind, EffectKind};
use crate::services::effects::parameters::{MasterPacket, MemberPacket};
use crate::services::skills::gm::{self, Change};
use crate::services::tests::fixture::ShipItemSpec;
use crate::space;
use crate::space::system::boarding::BoardRefusal;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CARGO, FLAG_MED_SLOT_FIRST, GROUP_DREADNOUGHT, GROUP_FORCE_AUX, GROUP_LANCER_DREADNOUGHT};
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE_AMOUNT, ATTR_CAPACITY, ATTR_CONSUMPTION_QUANTITY, ATTR_CONSUMPTION_TYPE, ATTR_DAMAGE_MULTIPLIER, ATTR_DISALLOW_EARLY_DEACTIVATION, ATTR_DURATION, ATTR_MAX_VELOCITY, ATTR_RELOAD_TIME, ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS, ATTR_SHIELD_BONUS, ATTR_STRUCTURE_DAMAGE_AMOUNT, EFFECT_ARMOR_REPAIR, EFFECT_SHIELD_BOOSTING, EFFECT_SIEGE, EFFECT_STRUCTURE_REPAIR, EFFECT_TARGET_ATTACK};
use std::sync::{Arc, atomic::AtomicBool};

const BOUNDED_MS: f64 = 1_500.0;

fn dynamic_result(sde: &Sde) -> i32 {
    sde.types.iter().find_map(|ty| {
        let mutation = sde.dynamic_items.get(ty.type_id)?;
        mutation.mappings.values().copied().find(|result| sde.types.get(*result).is_some_and(|data|
            data.published == Some(true) && data.category_id == Some(CATEGORY_MODULE)
            && data.effects().iter().any(|effect| effect.effect_id == EFFECT_SIEGE)))
    }).expect("published dynamic siege result")
}

fn siege_mutator(sde: &Sde, source: i32, result: i32) -> i32 {
    sde.types.iter().find_map(|ty| {
        let mutation = sde.dynamic_items.get(ty.type_id)?;
        (mutation.result_type(source) == Some(result)).then_some(ty.type_id)
    }).expect("mutator for the published siege source")
}

fn fit_groups(sde: &Sde, module_type: i32) -> Vec<i32> {
    (1..).map_while(|index| sde.dogma.attributes_by_name(&format!("canFitShipGroup{index:02}")).next())
        .filter_map(|attribute| sde.types.get(module_type).and_then(|data| data.attribute(attribute.attribute_id)).filter(|value| *value > 0.0))
        .map(|value| value as i32).collect()
}

fn hull_in(sde: &Sde, groups: &[i32]) -> i32 {
    sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP)
        && groups.contains(&ty.group_id.unwrap_or(0)) && ty.attribute(ATTR_STRUCTURE_HP).is_some_and(|value| value > 0.0))
        .map(|ty| ty.type_id).expect("published hull in the declared groups")
}

fn consumer(sde: &Sde, effect: i32, groups: &[i32], slot: i32) -> Option<(i32, i32)> {
    let captured = match kind(sde, effect) {
        Some(EffectKind::Turret) => ATTR_DAMAGE_MULTIPLIER,
        Some(EffectKind::LocalRepair { amount_attr, .. }) => amount_attr,
        _ => return None,
    };
    let modifiers = sde.dogma.effect(EFFECT_SIEGE).expect("siege modifiers").modifiers();
    let scoped = modifiers.iter().filter(|row| row.domain == "shipID" && row.modified == captured
        && (row.func == "LocationRequiredSkillModifier" || row.func == "LocationGroupModifier"));
    sde.types.iter().find_map(|ty| {
        if !(ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE) && ty.effects().iter().any(|linked| linked.effect_id == effect)) { return None; }
        let skills = ATTR_REQUIRED_SKILLS.iter().filter_map(|attribute| ty.attribute(*attribute).filter(|value| *value > 0.0).map(|value| value as i32));
        if !scoped.clone().any(|row| row.group.is_some_and(|group| ty.group_id == Some(group)) || row.skill.is_some_and(|skill| skills.clone().any(|required| required == skill))) { return None; }
        let fits = fit_groups(sde, ty.type_id);
        if !fits.is_empty() && fits.iter().all(|group| !groups.contains(group)) { return None; }
        let (_, _, first) = crate::services::effects::fitting::rack(&sde.types, ty.type_id)?;
        Some((ty.type_id, first + slot))
    })
}

fn charge_for(sde: &Sde, module_type: i32) -> i32 {
    let module = sde.types.get(module_type).expect("consumer type");
    let groups = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].into_iter()
        .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|attribute| module.attribute(attribute.attribute_id))
        .filter(|value| *value > 0.0).map(|value| value as i32).collect::<Vec<_>>();
    let size = sde.dogma.attributes_by_name("chargeSize").next().and_then(|attribute| module.attribute(attribute.attribute_id)).filter(|value| *value > 0.0);
    sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
        && groups.contains(&ty.group_id.unwrap_or(0))
        && size.is_none_or(|wanted| ty.attribute(sde.dogma.attributes_by_name("chargeSize").next().expect("charge size").attribute_id) == Some(wanted)))
        .map(|ty| ty.type_id).expect("published compatible charge")
}

async fn bound_clock(db: &crate::db::Db, sde: &Sde, item: i64, effect: i32) {
    let duration = sde.dogma.effect(effect).and_then(|row| row.duration_attribute_id).expect("effect duration attribute");
    item_dogma::set_override(db, item, duration, BOUNDED_MS).await.unwrap();
}
fn fuel_spec(sde: &Sde, module_type: i32) -> ShipItemSpec<'static> {
    ShipItemSpec { type_id: sde.types.get(module_type).expect("siege type").attribute(ATTR_CONSUMPTION_TYPE).expect("fuel type") as i32,
        flag_id: FLAG_CARGO, quantity: 1, singleton: false, online: false, attributes: &[] }
}

async fn prepare_fuel(space: &InSpace, db: &crate::db::Db, module: i64, stack: i64, cycles: i64) -> i64 {
    let units = effective_units(space, module).await;
    let quantity = units.checked_mul(cycles).expect("fuel quantity");
    db.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&stack, &quantity]).await.unwrap();
    units
}

async fn mutated_siege(space: &mut InSpace, sde: &Sde, db: &crate::db::Db, source: i32) -> i64 {
    let result = dynamic_result(sde);
    let mutator_type = siege_mutator(sde, source, result);
    let (owner, ship) = (space.fixture.character, space.fixture.ship);
    let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,$6) RETURNING item_id";
    let source_item: i64 = db.query_one(insert, &[&source, &owner, &ship, &FLAG_CARGO, &1_i64, &true]).await.unwrap().get("item_id");
    space.fixture.items.push(source_item);
    let mutator: i64 = db.query_one(insert, &[&mutator_type, &owner, &ship, &FLAG_CARGO, &1_i64, &false]).await.unwrap().get("item_id");
    space.fixture.items.push(mutator);
    let created = crate::services::dispatch(&mut space.client, &crate::services::Call {
        service: "dynamicItemService", method: "CreateDynamicItem",
        args: &[Value::Int(mutator), Value::Int(source_item)], kwargs: &Value::Dict(vec![]), bound: false, bind_params: None,
    }).await.expect("CreateDynamicItem");
    let item = created.as_int().expect("created item");
    space.fixture.items.push(item);
    let stored = db.query_opt("SELECT result_type_id FROM game.dynamic_items WHERE item_id = $1", &[&item]).await.unwrap().expect("provenance");
    assert_eq!(stored.get::<_, i32>("result_type_id"), result);
    item
}





async fn running(space: &mut InSpace, module: i64) -> (Arc<AtomicBool>, i64) {
    until(&space.client.world, space.system, 5_000, || matches!(record(module), Record::Running)).await;
    match combat::state().active_cycles.get(&module) {
        Some(ActiveCycle::Running { generation, start, .. }) => (generation.clone(), *start),
        _ => panic!("running"),
    }
}
async fn wait_movement(space: &InSpace, ready: fn(&space::Ball) -> bool) {
    let (world, system, ship) = (&space.client.world, space.system, space.fixture.ship);
    let deadline = now_ms() + 5_000;
    while now_ms() < deadline {
        if world.with(system, move |s| s.ball(ship).is_some_and(ready)).await.unwrap() { return; }
        assert!(world.after(system, 100).await, "system actor stopped");
    }
    assert!(world.with(system, move |s| s.ball(ship).is_some_and(ready)).await.unwrap(), "movement not settled within 5000 ms");
}

async fn ship_call(space: &mut InSpace, method: &str, args: &[Value]) -> CallResult {
    let bind = Value::list([Value::Int(space.system), Value::Int(i64::from(infinity_core::consts::GROUP_SOLAR_SYSTEM))]);
    crate::services::dispatch(&mut space.client, &crate::services::Call {
        service: "ship", method, args, kwargs: &Value::Dict(vec![]), bound: true, bind_params: Some(&bind),
    }).await
}

async fn retired(space: &mut InSpace, module: i64, generation: &Arc<AtomicBool>) {
    let mut notices = Vec::new();
    until(&space.client.world, space.system, 8_000, || {
        notices.extend(drain(&mut space.rx));
        let gone = combat::state().active_cycles.get(&module).is_none_or(|current| !matches!(current, ActiveCycle::Running { generation: live, .. } if Arc::ptr_eq(live, generation)));
        gone && godma(&notices, module, i64::from(EFFECT_SIEGE), 0) >= 1
    }).await;
}

async fn fresh_health(world: &crate::space::World, system: i64, ship: i64) -> f64 {
    condition(world, system, ship).await.now.iter().sum()
}

async fn captured(space: &InSpace, sde: &Sde, db: &crate::db::Db, module: i64, module_type: i32, effect: i32) -> Cycle {
    let resolved = crate::services::effects::registry::resolve(sde, effect).expect("consumer effect");
    let generation = Arc::new(AtomicBool::new(false));
    let flag = crate::services::effects::fitting::rack(&sde.types, module_type).expect("rack").2;
    let cx = Ctx { db: db.clone(), world: space.client.world.clone(), notify: space.client.notify_tx.clone(), char_id: space.fixture.character, ship_id: space.fixture.ship, system_id: space.system };
    let presented = resolved.id;
    let cycle = Cycle { cx, modules: vec![Module { id: module, type_id: module_type, flag }], presented_effect: presented, effect: resolved, target: None, repeat: 1, stop: generation.clone(),
        owner: ActivationOwner { master: module, generation, armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: space.system },
        started: 0, charge_type: None, cycles: 0, duration_ms: BOUNDED_MS, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false, source_membership: None, target_type: None,
        inputs: crate::services::effects::parameters::CycleInputs::default() };
    effects_fixture::capture_cycle(cycle).await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn published_variants_start_once_and_retire_owned_generations() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let links = published_links(&sde, EFFECT_SIEGE);
    let result = dynamic_result(&sde);
    assert!(links.contains(&result), "the dynamic result is one published link");
    let ordinary_links = links.iter().copied().filter(|ty| *ty != result).collect::<Vec<_>>();
    assert_eq!(ordinary_links.len(), 3, "three ordinary published modules link siege");
    for module_type in ordinary_links {
        let groups = fit_groups(&sde, module_type);
        assert!(groups.contains(&GROUP_DREADNOUGHT) && groups.contains(&GROUP_LANCER_DREADNOUGHT), "ordinary siege fits both dreadnought groups");
        let fuel = fuel_spec(&sde, module_type);
        run_one(module_type, std::slice::from_ref(&fuel)).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn siege_ordinary_i_pays_benefits_and_retires_its_generation() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let result = dynamic_result(&sde);
    let ordinary = published_links(&sde, EFFECT_SIEGE).into_iter().filter(|ty| *ty != result).min_by_key(|ty| {
        let data = sde.types.get(*ty).expect("type");
        ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).filter_map(|(&skill, level)| data.attribute(skill).filter(|value| *value > 0.0).map(|_| data.attribute(level).unwrap_or(0.0) as i32)).sum::<i32>()
    }).expect("lowest declared skill");
    let fuel = fuel_spec(&sde, ordinary);
    run_one(ordinary, std::slice::from_ref(&fuel)).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn siege_dynamic_result_inherits_source_rows_and_retires_its_paid_generation() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let source = published_links(&sde, EFFECT_SIEGE).into_iter().find(|ty| *ty != dynamic_result(&sde)).expect("ordinary source");
    let fuel = fuel_spec(&sde, source);
    let mut space = in_space(&db, &sde, source, EFFECT_SIEGE, &[], std::slice::from_ref(&fuel), None).await;

    let result = crate::tests::catch_scenario(async {
        let owned = mutated_siege(&mut space, &sde, &db, source).await;
        let rows = db.query("SELECT attribute_id FROM game.item_attributes WHERE item_id = $1", &[&owned]).await.unwrap();
        assert!(rows.iter().any(|row| row.get::<_, i32>("attribute_id") == ATTR_DURATION));
        assert!(rows.iter().any(|row| row.get::<_, i32>("attribute_id") == ATTR_CONSUMPTION_TYPE));
        bound_clock(&db, &sde, owned, EFFECT_SIEGE).await;
        let volume = |type_id, singleton, quantity| crate::services::item_volume::item_volume(&sde, type_id, singleton, quantity, None).unwrap();
        let current = space.client.world.with(space.system, { let ship = space.fixture.ship; move |s| s.resident_fit(ship, |fit, _| fit.value(crate::services::fit::Key::Ship, ATTR_CAPACITY)).expect("resident cargo capacity") }).await.unwrap();
        let needed = (volume(source, true, 1) + volume(dynamic_result(&sde), true, 1) + volume(fuel.type_id, false, effective_units(&space, space.module).await)).max(current);
        item_dogma::set_override(&db, space.fixture.ship, ATTR_CAPACITY, needed).await.unwrap();
        effects_fixture::publish_fit(&db, &space.client.world, space.system, space.fixture.character, space.fixture.ship).await;
        let flag = crate::services::effects::fitting::rack(&sde.types, dynamic_result(&sde)).expect("dynamic rack").2;
        unfit(&mut space.client, space.module, space.fixture.ship).await.expect("ordinary module leaves its slot");
        let bind = Value::tuple([Value::Int(space.fixture.ship)]);
        let kwargs = Value::dict([("flag", Value::Int(i64::from(flag)))]);
        crate::services::dispatch(&mut space.client, &crate::services::Call {
            service: crate::services::inventory::INVENTORY, method: "Add",
            args: &[Value::Int(owned), Value::Int(space.fixture.ship)], kwargs: &kwargs, bound: true, bind_params: Some(&bind),
        }).await.expect("created item uses the fitting path");
        module_online(&space.client, space.fixture.ship, owned, true).await.expect("created item comes online");
        effects_fixture::publish_fit(&db, &space.client.world, space.system, space.fixture.character, space.fixture.ship).await;
        let stack = space.fixture.items[1];
        pay_and_retire(&mut space, &sde, &db, owned, stack).await;
    }).await;
    teardown(&db, space).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

async fn run_one(module_type: i32, extra: &[ShipItemSpec<'_>]) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let mut space = in_space(&db, &sde, module_type, EFFECT_SIEGE, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], extra, None).await;
    let result = crate::tests::catch_scenario(async {
        let actual = db.query_one("SELECT type_id FROM game.items WHERE item_id = $1", &[&space.module]).await.unwrap().get::<_, i32>("type_id");
        assert_eq!(actual, module_type, "the explicit ordinary module is fitted");
        let (module, stack) = (space.module, space.fixture.items[1]);
        pay_and_retire(&mut space, &sde, &db, module, stack).await;
    }).await;
    teardown(&db, space).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
async fn pay_and_retire(space: &mut InSpace, sde: &Sde, db: &crate::db::Db, module: i64, stack: i64) {
    let (system, ship) = (space.system, space.fixture.ship);
    prepare_fuel(space, db, module, stack, 1).await;
    let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
    space.client.world.observe(system, space.fixture.character, ship, destiny).await.unwrap();
    let _ = drain(&mut space.rx);
    let base = space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_MAX_VELOCITY))).await.unwrap().unwrap();
    let name = effect_name(sde, EFFECT_SIEGE);
    activate(space, &name, module).await;
    let (generation, _) = running(space, module).await;
    assert!(combat::state().active.contains_key(&module), "acceptance leaves the active generation");
    assert!(active_effects(&mut space.client, module).await.contains(&i64::from(EFFECT_SIEGE)));
    assert_eq!(quantity(db, stack).await, 0, "the accepted cycle pays its fuel");
    assert_eq!(fx_count(&mut events, module, 1), 1, "one fresh current effect start");
    assert_eq!(godma(&drain(&mut space.rx), module, i64::from(EFFECT_SIEGE), 1), 1);
    let changed = space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_MAX_VELOCITY))).await.unwrap().unwrap();
    assert_ne!(changed, base, "the mode changes fitted speed");
    effects_fixture::dogma_call(&mut space.client, "Deactivate", &[Value::Int(module), Value::str(&name)]).await.ok().unwrap();
    assert!(matches!(combat::state().active_cycles.get(&module), Some(ActiveCycle::Running { generation: live, .. }) if Arc::ptr_eq(live, &generation)), "the stop keeps the paid generation");
    assert_eq!(space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_MAX_VELOCITY))).await.unwrap().unwrap(), changed, "the paid benefit remains");
    retired(space, module, &generation).await;
    assert_eq!(fx_count(&mut events, module, 0), 1, "the original generation publishes one stop");
    assert_eq!(space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_MAX_VELOCITY))).await.unwrap().unwrap(), base, "retirement restores fitted speed");
    space.client.world.unobserve(space.fixture.character);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn siege_real_xl_turret_and_local_repair_consume_fitted_values() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let groups = fit_groups(&sde, module_type(&sde, EFFECT_SIEGE));
    let (turret_type, turret_flag) = consumer(&sde, EFFECT_TARGET_ATTACK, &groups, 1).expect("capital turret");
    let repair_effect = [EFFECT_SHIELD_BOOSTING, EFFECT_ARMOR_REPAIR, EFFECT_STRUCTURE_REPAIR].into_iter().find(|effect| consumer(&sde, *effect, &groups, 0).is_some()).expect("capital local repair");
    let (repair_type, repair_flag) = consumer(&sde, repair_effect, &groups, 1).expect("capital local repair");
    let charge = charge_for(&sde, turret_type);
    let module_type = module_type(&sde, EFFECT_SIEGE);
    let fuel = fuel_spec(&sde, module_type);
    scenario_spec(EFFECT_SIEGE, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], &[
        ShipItemSpec { type_id: turret_type, flag_id: turret_flag, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: repair_type, flag_id: repair_flag, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: charge, flag_id: FLAG_CARGO, quantity: 1, singleton: false, online: false, attributes: &[] },
        fuel,
    ], async |space, sde, db| {
        let (system, module, ship, character) = (space.system, space.module, space.fixture.ship, space.fixture.character);
        let (turret, repair, charge_item, fuel_stack) = (space.fixture.items[1], space.fixture.items[2], space.fixture.items[3], space.fixture.items[4]);
        bound_clock(db, sde, turret, EFFECT_TARGET_ATTACK).await;
        bound_clock(db, sde, repair, repair_effect).await;
        item_dogma::set_override(db, turret, ATTR_RELOAD_TIME, BOUNDED_MS).await.unwrap();
        item_dogma::set_override(db, turret, ATTR_DAMAGE_MULTIPLIER, 1.0).await.unwrap();
        let amount = match repair_effect { EFFECT_SHIELD_BOOSTING => ATTR_SHIELD_BONUS, EFFECT_ARMOR_REPAIR => ATTR_ARMOR_DAMAGE_AMOUNT, _ => ATTR_STRUCTURE_DAMAGE_AMOUNT };
        item_dogma::set_override(db, repair, amount, 1.0).await.unwrap();
        prepare_fuel(space, db, module, fuel_stack, 1).await;
        effects_fixture::publish_fit(db, &space.client.world, system, character, ship).await;
        crate::services::combat::targeting_charges::load_ammo_core(&mut space.client, ship, vec![turret], vec![charge_item], Some(ship)).await.expect("loaded charge");
        until(&space.client.world, system, 5_000, || !combat::state().reloading.contains_key(&turret)).await;
        let (hull, pos) = space.client.world.with(system, move |s| { let ball = s.ball(ship).expect("ship"); let mut next = ball.pos; next[0] += f64::from(ball.radius) * 2.0; (ball.type_id, next) }).await.unwrap();
        let target_fixture = crate::services::tests::fixture::create_ship(db, hull, crate::services::tests::fixture::ShipLocation::Space { system }, &[]).await.unwrap();
        let target = target_fixture.ship;
        let result = crate::tests::catch_scenario(async {
        let mut target_ball = effects_fixture::ball(target, hull, target_fixture.character, None, pos[0]); target_ball.pos = pos; target_ball.old_pos = pos;
        effects_fixture::admit(db, &space.client.world, system, target_ball).await;
        set_locked_target(character, target, true);
        let loaded = crate::services::fit::Fit::load_in_space(db, &space.client.world, system, character, ship).await.unwrap();
        crate::services::combat::tests::remote_repair_db::install_half(&space.client.world, system, ship, loaded).await;
        let damaged = fresh_health(&space.client.world, system, ship).await;
        let layer = match kind(sde, repair_effect) { Some(EffectKind::LocalRepair { layer, .. }) => layer, _ => panic!("selected local repair") };
        let installed = condition(&space.client.world, system, ship).await;
        assert!(installed.now[layer] < installed.stats.max[layer], "the damaged layer has repair headroom");
        let baseline_repair = captured(space, sde, db, repair, repair_type, repair_effect).await;
        let before = condition(&space.client.world, system, target).await.now.iter().sum::<f64>();
        let baseline_turret = captured(space, sde, db, turret, turret_type, EFFECT_TARGET_ATTACK).await;
        let name = effect_name(sde, EFFECT_SIEGE);
        activate(space, &name, module).await;
        let (generation, _) = running(space, module).await;
        let turret_cycle = captured(space, sde, db, turret, turret_type, EFFECT_TARGET_ATTACK).await;
        assert!(matches!(record(module), Record::Running), "turret capture uses the paid generation");
        let turret_inputs = turret_cycle.inputs.members.iter().find_map(|member| match &member.packet { MemberPacket::Turret(inputs) => Some(inputs.raw), _ => None }).expect("captured turret");
        let baseline_inputs = baseline_turret.inputs.members.iter().find_map(|member| match &member.packet { MemberPacket::Turret(inputs) => Some(inputs.raw), _ => None }).expect("baseline turret");
        assert!(turret_inputs.iter().sum::<f64>() > 0.0 && turret_inputs.iter().sum::<f64>() > baseline_inputs.iter().sum::<f64>(), "siege changes the captured turret operands");
        let repair_cycle = captured(space, sde, db, repair, repair_type, repair_effect).await;
        assert!(matches!(combat::state().active_cycles.get(&module), Some(ActiveCycle::Running { generation: live, .. }) if Arc::ptr_eq(live, &generation)), "repair capture uses the paid generation");
        let captured_amount = match repair_cycle.inputs.frame.as_ref().map(|frame| &frame.packet) { Some(MasterPacket::Repair(inputs)) => inputs.base, _ => panic!("captured repair") };
        let baseline_amount = match baseline_repair.inputs.frame.as_ref().map(|frame| &frame.packet) { Some(MasterPacket::Repair(inputs)) => inputs.base, _ => panic!("baseline repair") };
        assert!(captured_amount > 1.0 && captured_amount > baseline_amount, "siege changes the captured repair amount");
            effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(turret), Value::str(&effect_name(sde, EFFECT_TARGET_ATTACK)), Value::Int(target), Value::Int(100)]).await.expect("turret");
            effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(repair), Value::str(&effect_name(sde, repair_effect)), Value::None, Value::Int(100)]).await.expect("repair");
            for _ in 0..80 { if fresh_health(&space.client.world, system, target).await < before { break; } assert!(space.client.world.after(system, 100).await); }
            assert!(fresh_health(&space.client.world, system, target).await < before, "turret changes health");
            for _ in 0..80 { if fresh_health(&space.client.world, system, ship).await > damaged { break; } assert!(space.client.world.after(system, 100).await); }
            assert!(fresh_health(&space.client.world, system, ship).await > damaged, "repair changes health");
        }).await;
        set_locked_target(character, target, false);
        crate::services::tests::fixture::cleanup_ship(db, Some(&space.client.world), target_fixture).await.unwrap();
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn siege_repeats_pay_fuel_and_shortage_retires_without_restart() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let module_type = module_type(&sde, EFFECT_SIEGE);
    let fuel = fuel_spec(&sde, module_type);
    scenario_spec(EFFECT_SIEGE, &[], &[fuel], async |space, sde, db| {
        let (system, module, ship) = (space.system, space.module, space.fixture.ship);
        let stack = space.fixture.items[1];
        let units = prepare_fuel(space, db, module, stack, 2).await;
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        space.client.world.observe(system, space.fixture.character, ship, destiny).await.unwrap();
        let _ = drain(&mut space.rx);
        let name = effect_name(sde, EFFECT_SIEGE);
        activate(space, &name, module).await;
        let (generation, started) = running(space, module).await;
        assert_eq!(quantity(db, stack).await, units);
        until(&space.client.world, system, 8_000, || now_ms() >= started + DURATION_MS as i64 + 400).await;
        assert_eq!(quantity(db, stack).await, 0, "the repeat pays the second requirement");
        assert!(matches!(combat::state().active_cycles.get(&module), Some(ActiveCycle::Running { generation: live, .. }) if Arc::ptr_eq(live, &generation)));
        assert_eq!(fx_count(&mut events, module, 1), 1);
        retired(space, module, &generation).await;
        assert!(!space.client.world.with(system, move |s| crate::services::effects::active_cycle::mode_locked(s, ship)).await.unwrap());
        assert_eq!(effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(module), Value::str(&name), Value::None, Value::Int(100)]).await.ok(), Some(Value::None));
        let mut notices = Vec::new();
        until(&space.client.world, system, 8_000, || { notices.extend(drain(&mut space.rx)); record(module) == Record::None && notices.iter().any(|(event, _)| event == "OnEffectMessage") }).await;
        assert_eq!(quantity(db, stack).await, 0);
        assert_eq!(godma(&notices, module, i64::from(EFFECT_SIEGE), 1), 0);
        space.client.world.unobserve(space.fixture.character);
    }).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn siege_boundary_refusals_preserve_the_paid_cycle() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let other = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
        && crate::services::effects::fitting::rack(&sde.types, ty.type_id).is_some_and(|(_, _, first)| first == FLAG_MED_SLOT_FIRST)).map(|ty| ty.type_id).expect("unrelated module");
    let host_type = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP)).map(|ty| ty.type_id).expect("host");
    let fuel = fuel_spec(&sde, module_type(&sde, EFFECT_SIEGE));
    scenario_spec(EFFECT_SIEGE, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], &[
        ShipItemSpec { type_id: other, flag_id: FLAG_MED_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
        fuel,
    ], async |space, sde, db| {
        let (system, module, ship, character) = (space.system, space.module, space.fixture.ship, space.fixture.character);
        let (unrelated, fuel_stack) = (space.fixture.items[1], space.fixture.items[2]);
        let units = prepare_fuel(space, db, module, fuel_stack, 1).await;
        let name = effect_name(sde, EFFECT_SIEGE);
        let args = activate_args(space, &name);
        let row = space.client.world.with(system, move |s| { let ball = s.ball(ship).expect("ship"); crate::dao::items::space_ships::FreeShip { item_id: ball.id, type_id: ball.type_id, owner_id: ball.owner_id, corporation_id: ball.corp_id, alliance_id: ball.alliance_id, pos: ball.pos } }).await.unwrap();
        let native = crate::services::ship::balls::piloted(db, space.client.world.sde().clone(), &row, character, 0.0).await.unwrap().ball;
        space.client.world.with(system, move |s| s.command(ship, move |ball, _, _| { ball.free = native.free; ball.massive = native.massive; ball.max_vel = native.max_vel; ball.mass = native.mass; ball.agility = native.agility; Vec::new() })).await.unwrap();
        let pos = space.client.world.with(system, move |s| s.ball(ship).expect("ship").pos).await.unwrap();
        let destination = crate::space::system::warp::WarpDestination::Exact([pos[0] + 1_000_000_000.0, pos[1], pos[2]]);
        assert!(crate::services::ship::warp_pilot(db, &space.client.world, system, character, ship, destination, None, None).await.unwrap());
        space.client.world.with(system, move |s| s.command(ship, |ball, _, _| { ball.vel = [f64::from(ball.max_vel), 0.0, 0.0]; Vec::new() })).await.unwrap();
        wait_movement(space, |ball| ball.is_warping()).await;
        assert!(space.client.world.with(system, move |s| s.ball(ship).is_some_and(|ball| ball.mode == space::MODE_WARP && ball.is_warping())).await.unwrap(), "the real warp persists");
        assert!(matches!(effects_fixture::dogma_call(&mut space.client, "Activate", &args).await, Err(crate::services::CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_DENIED_ACTIVATE_IN_WARP));
        assert!(record(module) == Record::None, "warp admission spends no paid generation");
        assert_eq!(quantity(db, fuel_stack).await, units, "warp admission pays no fuel");
        space.client.world.with(system, move |s| s.command(ship, |ball, _, _| { ball.stop(); Vec::new() })).await.unwrap();
        wait_movement(space, |ball| ball.mode == space::MODE_STOP).await;
        assert!(space.client.world.with(system, move |s| s.ball(ship).is_some_and(|ball| ball.mode == space::MODE_STOP)).await.unwrap(), "the real stop persists");
        let cloak = Arc::new(AtomicBool::new(false));
        assert!(space.client.world.with(system, { let cloak = cloak.clone(); move |s| s.begin_module_cloak(ship, unrelated, cloak, 1_000, 1_000, |_| true, None) }).await.unwrap());
        assert!(effects_fixture::dogma_call(&mut space.client, "Activate", &args).await.is_err(), "module cloak refuses admission");
        space.client.world.with(system, { let cloak = cloak.clone(); move |s| s.stop_module_cloak(ship, unrelated, &cloak) }).await.unwrap();
        activate(space, &name, module).await;
        let (generation, _) = running(space, module).await;
        let changed = space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_MAX_VELOCITY))).await.unwrap().unwrap();
        effects_fixture::dogma_call(&mut space.client, "Deactivate", &[Value::Int(module), Value::str(&name)]).await.ok().unwrap();
        assert!(matches!(record(module), Record::Running));
        assert_eq!(space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_MAX_VELOCITY))).await.unwrap().unwrap(), changed);
        assert!(module_online(&space.client, ship, module, false).await.is_err());
        assert!(unfit(&mut space.client, module, ship).await.is_err());
        assert!(module_online(&space.client, ship, unrelated, false).await.is_ok());
        let refused = space.client.world.with(system, move |s| s.authorize_own_hull_exit(character, ship)).await.unwrap();
        assert_eq!(refused.err(), Some(BoardRefusal::Unsafe), "eject, board and store share this refusal");
        space.client.session.apply(vec![("charid", Value::Int(character))]);
        assert!(ship_call(space, "Eject", &[]).await.is_err());
        let host: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,0,1,true) RETURNING item_id", &[&host_type, &character, &system]).await.unwrap().get("item_id");
        let mut host_ball = effects_fixture::ball(host, host_type, character, None, 0.0); host_ball.pos = [1_000.0, 0.0, 0.0]; host_ball.old_pos = host_ball.pos;
        effects_fixture::admit(db, &space.client.world, system, host_ball).await;
        assert!(ship_call(space, "StoreVessel", &[Value::Int(host)]).await.is_err());
        let stored: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&host_type, &character, &host, &infinity_core::consts::FLAG_SHIP_HANGAR]).await.unwrap().get("item_id");
        assert!(ship_call(space, "BoardStoredShip", &[Value::Int(host), Value::Int(stored)]).await.is_err());
        let station = sde.map.stations().find(|row| row.solar_system_id == Some(system)).expect("station").station_id;
        let celestial = sde.map.celestial(station).expect("celestial");
        let position = [celestial.x.unwrap(), celestial.y.unwrap(), celestial.z.unwrap()];
        space.client.world.with(system, move |s| { s.command(ship, move |ball, _, _| { ball.pos = position; Vec::new() }); }).await.unwrap();
        let identity = crate::services::identity::Identity::of_client(&space.client);
        assert!(crate::services::ship::dock(&identity, &space.client.db, &space.client.world, Some(system), station, None::<&mut crate::client::Client>).await.is_err());
        assert!(matches!(combat::state().active_cycles.get(&module), Some(ActiveCycle::Running { generation: live, .. }) if Arc::ptr_eq(live, &generation)));
    }).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn siege_forced_retirement_and_successor_fences_preserve_current_owner() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let fuel = fuel_spec(&sde, module_type(&sde, EFFECT_SIEGE));
    scenario_spec(EFFECT_SIEGE, &[(ATTR_DISALLOW_EARLY_DEACTIVATION, 1.0)], std::slice::from_ref(&fuel), async |space, _sde, db| {
        let (system, module, ship, character) = (space.system, space.module, space.fixture.ship, space.fixture.character);
        prepare_fuel(space, db, module, space.fixture.items[1], 1).await;
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        space.client.world.observe(system, character, ship, destiny).await.unwrap();
        let name = effect_name(&space.client.sde, EFFECT_SIEGE);
        activate(space, &name, module).await;
        let (original, _) = running(space, module).await;
        space.client.world.with(system, move |s| { s.fitted_module_online(ship, module, false); }).await.unwrap();
        until(&space.client.world, system, 5_000, || !matches!(combat::state().active_cycles.get(&module), Some(ActiveCycle::Running { generation, .. }) if Arc::ptr_eq(generation, &original))).await;
        assert_eq!(fx_count(&mut events, module, 0), 1);
        let successor = Arc::new(AtomicBool::new(false));
        let membership = space.client.world.with(system, move |s| s.persistent_generation(ship)).await.unwrap().unwrap();
        space.client.world.with(system, { let successor = successor.clone(); move |s| {
            s.fitted_module_online(ship, module, true);
            crate::services::tests::fixture::install_running_mode(module, ship, character, i64::from(EFFECT_SIEGE), membership, &successor);
            combat::state().active.insert(module, combat::ActiveModule { char_id: character, stop: successor.clone(), forced_inactive: false });
            s.fitted_module_active(ship, module, true);
            let stale = { let mut state = combat::state(); crate::services::effects::active_cycle::terminate(&mut state, module, &original) };
            if let Some(terminal) = stale { crate::services::effects::active_cycle::apply_terminal(s, terminal); }
        }}).await.unwrap();
        assert!(matches!(record(module), Record::Running));
        space.client.world.with(system, move |s| crate::services::effects::active_cycle::ship_departure(s, ship)).await.unwrap();
        assert_eq!(record(module), Record::None);
        space.client.world.unobserve(character);
    }).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn siege_fitting_limits_and_dynamic_inheritance_use_declared_data() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let links = published_links(&sde, EFFECT_SIEGE);
    let source = links.iter().copied().find(|ty| *ty != dynamic_result(&sde)).expect("ordinary source");
    let wrong = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP) && ty.group_id == Some(GROUP_FORCE_AUX)).map(|ty| ty.type_id).expect("force auxiliary");
    scenario_spec(EFFECT_SIEGE, &[], &[], async |space, sde, db| {
        let (module, ship, character) = (space.module, space.fixture.ship, space.fixture.character);
        let module_type = db.query_one("SELECT type_id FROM game.items WHERE item_id = $1", &[&module]).await.unwrap().get::<_, i32>("type_id");
        let fit = crate::services::fit::Fit::load(db, sde.clone(), character, ship).await.unwrap();
        let skill = ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).find_map(|(&skill, level)| {
            let id = fit.value(crate::services::fit::Key::Module(module), skill) as i32;
            (id > 0).then_some((id, fit.value(crate::services::fit::Key::Module(module), level) as i32))
        }).expect("declared skill");
        gm::apply(&mut space.client, character, Change::Set(vec![(skill.0, 0)])).await.unwrap();
        let unskilled = crate::services::fit::Fit::load(db, sde.clone(), character, ship).await.unwrap();
        assert!(crate::services::effects::fitting::required_skills(&unskilled, sde, crate::services::fit::Key::Module(module), module_type, true).is_err());
        gm::apply(&mut space.client, character, Change::Set(vec![(skill.0, skill.1)])).await.unwrap();
        db.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&ship, &wrong]).await.unwrap();
        assert!(crate::services::effects::fitting::validate_fit(db, sde.clone(), character, ship, module).await.is_err());
        db.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&ship, &hull_in(sde, &fit_groups(sde, module_type))]).await.unwrap();
        let second_flag = crate::services::effects::fitting::rack(&sde.types, module_type).expect("rack").2 + 1;
        let second: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&module_type, &character, &ship, &second_flag]).await.unwrap().get("item_id");
        assert!(crate::services::effects::fitting::validate_fit(db, sde.clone(), character, ship, second).await.is_err(), "the declared group cap refuses a second module");
        db.execute("DELETE FROM game.items WHERE item_id = $1", &[&second]).await.unwrap();
        let created = mutated_siege(space, sde, db, source).await;
        let rows = db.query("SELECT attribute_id, value FROM game.item_attributes WHERE item_id = $1", &[&created]).await.unwrap();
        let source_duration = sde.types.get(source).unwrap().attribute(ATTR_DURATION).expect("source duration");
        let source_fuel = sde.types.get(source).unwrap().attribute(ATTR_CONSUMPTION_QUANTITY).expect("source fuel");
        let value = |attribute: i32| rows.iter().find(|row| row.get::<_, i32>("attribute_id") == attribute).map(|row| row.get::<_, f64>("value"));
        assert_eq!(value(ATTR_DURATION), Some(source_duration));
        assert_eq!(value(ATTR_CONSUMPTION_QUANTITY), Some(source_fuel));
        assert!(value(ATTR_DISALLOW_EARLY_DEACTIVATION).is_some());

    }).await;
}
