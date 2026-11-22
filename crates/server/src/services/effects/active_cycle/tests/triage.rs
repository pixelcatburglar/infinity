//! Ordinary and Stellian triage share the repeating mode lifecycle. Local and
//! remote consumers, owner drones, lock limits, high-security refusal, fuel and
//! terminals use the loaded SDE and the parent in-space fixture.
use super::*;
use crate::services::combat::tests::remote_repair_db::{self, set_locked_target};
use crate::services::fit::{Fit, Key};
use crate::services::tests::fixture::{create_ship, ShipItemSpec, ShipLocation};
use crate::services::skills::gm::{self, Change};
use infinity_core::consts::{CATEGORY_DRONE, FLAG_CARGO, FLAG_DRONE_BAY, GROUP_SOLAR_SYSTEM};
use infinity_core::consts::CATEGORY_SKILL;
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE_AMOUNT, ATTR_ARMOR_HP, ATTR_CAPACITOR_CAPACITY, ATTR_CHARGE, ATTR_CONSUMPTION_QUANTITY, ATTR_CONSUMPTION_TYPE, ATTR_CPU_OUTPUT, ATTR_DRONE_BANDWIDTH, ATTR_DRONE_BANDWIDTH_USED, ATTR_DRONE_CAPACITY, ATTR_EM_DAMAGE, ATTR_HI_SLOTS, ATTR_LOW_SLOTS, ATTR_MAX_ACTIVE_DRONES, ATTR_MAX_LOCKED_TARGETS, ATTR_MAX_RANGE, ATTR_MAX_TARGET_RANGE, ATTR_MED_SLOTS, ATTR_POWER_OUTPUT, ATTR_POWER_TRANSFER_AMOUNT, ATTR_RECHARGE_RATE, ATTR_SCAN_GRAVIMETRIC_STRENGTH, ATTR_SCAN_LADAR_STRENGTH, ATTR_SCAN_MAGNETOMETRIC_STRENGTH, ATTR_SCAN_RADAR_STRENGTH, ATTR_SCAN_RESOLUTION, ATTR_SHIELD_BONUS, ATTR_SHIELD_CAPACITY, ATTR_SIGNATURE_RADIUS, ATTR_STRUCTURE_HP, EFFECT_ARMOR_REPAIR, EFFECT_REMOTE_ARMOR_REPAIRER, EFFECT_REMOTE_CAPACITOR_TRANSMITTER, EFFECT_REMOTE_ECM_FALLOFF, EFFECT_SHIELD_BOOSTING, EFFECT_STELLIAN_TRIAGE, EFFECT_TRIAGE};


fn module_spec<'a>(sde: &Sde, type_id: i32, slot: i32, attrs: &'a [(i32, f64)]) -> ShipItemSpec<'a> {
    let flag = crate::services::effects::fitting::rack(&sde.types, type_id).expect("module rack").2 + slot;
    ShipItemSpec { type_id, flag_id: flag, quantity: 1, singleton: true, online: true, attributes: attrs }
}

fn fuel_type(sde: &Sde, module_type: i32) -> i32 {
    sde.types.get(module_type).expect("triage type").attribute(ATTR_CONSUMPTION_TYPE).expect("fuel type") as i32
}

fn raw_units(sde: &Sde, module_type: i32) -> i64 {
    sde.types.get(module_type).and_then(|data| data.attribute(ATTR_CONSUMPTION_QUANTITY)).unwrap_or(0.0).ceil().max(1.0) as i64
}

async fn fitted(space: &InSpace, module: i64, attribute: i32) -> f64 {
    let (system, ship) = (space.system, space.fixture.ship);
    space.client.world.with(system, move |s| s.resident_fit(ship, |fit, _| fit.value(Key::Module(module), attribute))).await.unwrap().expect("resident module")
}

async fn ship_value(space: &InSpace, attribute: i32) -> f64 {
    let (system, ship) = (space.system, space.fixture.ship);
    space.client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.value(Key::Ship, attribute))).await.unwrap().expect("resident ship")
}

async fn health(space: &InSpace, ship: i64) -> [f64; 3] {
    remote_repair_db::condition(&space.client.world, space.system, ship).await.now
}

async fn wait_health_above(world: &crate::space::World, system: i64, ship: i64, layer: usize, before: f64, turns: usize) -> f64 {
    for _ in 0..turns {
        let read = remote_repair_db::condition(world, system, ship).await.now[layer];
        if read > before { return read; }
        assert!(world.after(system, 100).await, "actor stopped");
    }
    let read = remote_repair_db::condition(world, system, ship).await.now[layer];
    assert!(read > before, "health layer {layer} did not rise");
    read
}


fn drone_modifier(sde: &Sde, effect: i32) -> (i32, i32) {
    let row = sde.dogma.effect(effect).expect("triage").modifiers().iter()
        .find(|row| row.func == "OwnerRequiredSkillModifier" && row.modified == ATTR_EM_DAMAGE).expect("owner drone damage row");
    (row.modified, row.skill.expect("drone skill"))
}

async fn resident_drone(space: &InSpace, drone: i64, attribute: i32) -> f64 {
    let system = space.system;
    space.client.world.with(system, move |s| s.with_combat_fit(drone, |fit, key, _| fit.value(key, attribute))).await.unwrap().expect("resident drone")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn all_published_triage_variants_deliver_local_and_remote_benefits() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let ordinary = published_links(&sde, EFFECT_TRIAGE);
    let stellian = published_links(&sde, EFFECT_STELLIAN_TRIAGE);
    assert_eq!(ordinary.len(), 2, "ordinary triage publishes two modules");
    assert_eq!(stellian.len(), 1, "Stellian triage publishes one module");
    let placed = sde.map.systems().find(|row| !crate::services::crimewatch::highsec_system(&sde, row.solar_system_id)
        && sde.map.celestials(row.solar_system_id).any(|c| c.x.is_some() && c.y.is_some() && c.z.is_some()))
        .expect("eligible non-high-sec system").solar_system_id;
    let variants = [(EFFECT_TRIAGE, ordinary[0]), (EFFECT_TRIAGE, ordinary[1]), (EFFECT_STELLIAN_TRIAGE, stellian[0])];
    for (effect, type_id) in variants {
        let level = crate::services::skills::requirements::requirements(&sde, type_id).map(|(_, level)| level).max().expect("mode requirement");
        assert!(level == 1 || level == 5, "published triage requirement is level 1 or 5");
        let rows = sde.dogma.effect(effect).expect("triage").modifiers();
        let skills = |modified: i32| rows.iter().filter(|row| row.modified == modified).filter_map(|row| row.skill).collect::<Vec<_>>();
        let (armor_skills, shield_skills, cap_skills) = (skills(ATTR_ARMOR_DAMAGE_AMOUNT), skills(ATTR_SHIELD_BONUS), skills(ATTR_POWER_TRANSFER_AMOUNT));
        if effect == EFFECT_TRIAGE {
            assert!(!armor_skills.is_empty() && !shield_skills.is_empty() && !cap_skills.is_empty(), "ordinary triage declares armor, shield and capacitor bonuses");
        } else {
            assert!(!armor_skills.is_empty() && shield_skills.is_empty() && cap_skills.is_empty(), "Stellian declares armor bonuses and no shield or capacitor bonus");
        }
        let module_for = |family: i32, amount: i32, bonus: &[i32]| sde.types.iter().find(|ty| {
            ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
                && ty.effects().iter().any(|linked| linked.effect_id == family && linked.is_default)
                && ty.attribute(amount).is_some_and(|value| value > 0.0)
                && crate::services::effects::fitting::rack(&sde.types, ty.type_id).is_some()
                && (bonus.is_empty() || crate::services::skills::requirements::requirements(&sde, ty.type_id).any(|(required, _)| bonus.contains(&required)))
        }).map(|ty| ty.type_id);
        let local_shield = module_for(EFFECT_SHIELD_BOOSTING, ATTR_SHIELD_BONUS, &shield_skills).expect("local shield module");
        let local_armor = module_for(EFFECT_ARMOR_REPAIR, ATTR_ARMOR_DAMAGE_AMOUNT, &armor_skills).expect("local armor module");
        let remote_armor = module_for(EFFECT_REMOTE_ARMOR_REPAIRER, ATTR_ARMOR_DAMAGE_AMOUNT, &armor_skills).expect("remote armor module");
        let transmitter = module_for(EFFECT_REMOTE_CAPACITOR_TRANSMITTER, ATTR_POWER_TRANSFER_AMOUNT, &cap_skills).expect("capacitor transmitter");
        let duration_of = |family: i32| sde.dogma.effect(family).and_then(|row| row.duration_attribute_id).expect("consumer duration");
        let shield_clock = [(duration_of(EFFECT_SHIELD_BOOSTING), DURATION_MS)];
        let armor_clock = [(duration_of(EFFECT_ARMOR_REPAIR), DURATION_MS)];
        let remote_clock = [(duration_of(EFFECT_REMOTE_ARMOR_REPAIRER), DURATION_MS)];
        let cap_clock = [(duration_of(EFFECT_REMOTE_CAPACITOR_TRANSMITTER), DURATION_MS)];
        let mode_duration = sde.dogma.effect(effect).and_then(|row| row.duration_attribute_id).expect("mode duration");
        let fuel = ShipItemSpec { type_id: fuel_type(&sde, type_id), flag_id: FLAG_CARGO, quantity: raw_units(&sde, type_id) * 2, singleton: false, online: false, attributes: &[] };
        let specs = [module_spec(&sde, local_shield, 0, &shield_clock), module_spec(&sde, remote_armor, 1, &remote_clock), module_spec(&sde, transmitter, 2, &cap_clock), module_spec(&sde, local_armor, 0, &armor_clock), fuel];
        let initial = (effect == EFFECT_STELLIAN_TRIAGE).then_some(placed);
        let mut space = in_space(&db, &sde, type_id, effect, &[(mode_duration, 8_000.0)], &specs, initial).await;
        let result = crate::tests::catch_scenario(async {
            let (system, mode, ship, character) = (space.system, space.module, space.fixture.ship, space.fixture.character);
            if effect == EFFECT_STELLIAN_TRIAGE { assert_eq!(system, placed, "Stellian uses the explicit eligible system"); }
            let (shield_module, remote_module, cap_module, armor_module, stack) = (space.fixture.items[1], space.fixture.items[2], space.fixture.items[3], space.fixture.items[4], space.fixture.items[5]);
            let bound = space.client.world.with(system, move |s| s.resident_fit(ship, |fit, _| {
                [mode, shield_module, remote_module, cap_module, armor_module].map(|module| fit.source_type(Key::Module(module)))
            })).await.unwrap().expect("resident fit");
            assert_eq!(bound, [Some(type_id), Some(local_shield), Some(remote_armor), Some(transmitter), Some(local_armor)], "resident types are the selected modules");
            assert_eq!(quantity(&db, stack).await, fuel.quantity, "fuel is the explicit cargo item");
            let hull = space.client.world.ball(system, ship).await.expect("source").type_id;
            assert!(sde.types.get(hull).is_some_and(|ty| ty.attribute(ATTR_HI_SLOTS).unwrap_or(0.0) >= 3.0
                && ty.attribute(ATTR_MED_SLOTS).unwrap_or(0.0) >= 1.0 && ty.attribute(ATTR_LOW_SLOTS).unwrap_or(0.0) >= 1.0), "hull rack holds every consumer");
            let target_fixture = create_ship(&db, hull, ShipLocation::Space { system }, &[]).await.unwrap();
            let target = target_fixture.ship;
            let nested = crate::tests::catch_scenario(async {
                let pos = space.client.world.ball(system, ship).await.expect("source").pos;
                let mut ball = effects_fixture::ball(target, hull, target_fixture.character, Some(target_fixture.character), pos[0] + 1_000.0);
                ball.old_pos = ball.pos;
                effects_fixture::admit(&db, &space.client.world, system, ball).await;
                let residents = [(ship, character), (target, target_fixture.character)];
                for (item, owner) in residents {
                    for (attribute, value) in [(ATTR_SHIELD_CAPACITY, 100_000.0), (ATTR_ARMOR_HP, 100_000.0), (ATTR_STRUCTURE_HP, 100_000.0), (ATTR_CAPACITOR_CAPACITY, 100_000.0), (ATTR_RECHARGE_RATE, 1_000_000_000.0)] {
                        item_dogma::set_override(&db, item, attribute, value).await.unwrap();
                    }
                    effects_fixture::publish_fit(&db, &space.client.world, system, owner, item).await;
                    let fit = Fit::load_in_space(&db, &space.client.world, system, owner, item).await.unwrap();
                    remote_repair_db::install_half(&space.client.world, system, item, fit).await;
                    let seeded = space.client.world.with(system, move |s| {
                        let (capacity, tau) = s.resident_fit(item, |fit, _| (fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(Key::Ship, ATTR_RECHARGE_RATE))).expect("resident capacitor");
                        let now = crate::client::filetime_now();
                        let (_, full) = crate::services::combat::groups_health::fill_capacitor(item, capacity, tau, now);
                        crate::services::combat::groups_health::drain_warp_capacitor(item, capacity, tau, full * 0.5, now)
                    }).await.unwrap();
                    assert!((seeded.after - seeded.before * 0.5).abs() < 1.0 && seeded.drained > 0.0, "canonical capacitor is half the published capacity");
                }
                for (module, attribute, expected) in [(shield_module, shield_clock[0].0, DURATION_MS), (armor_module, armor_clock[0].0, DURATION_MS), (remote_module, remote_clock[0].0, DURATION_MS), (cap_module, cap_clock[0].0, DURATION_MS), (mode, mode_duration, 8_000.0)] {
                    let clocks = space.client.world.with(system, move |s| s.resident_fit(ship, |fit, _| {
                        (fit.base(Key::Module(module), attribute), fit.value(Key::Module(module), attribute))
                    })).await.unwrap().expect("resident effect clock");
                    assert!((clocks.0 - expected).abs() < 1.0, "effect-selected base clock: module={module} attr={attribute} base={} expected={expected}", clocks.0);
                    assert!(clocks.1.is_finite() && clocks.1 > 0.0, "effective clock after passive modifiers: module={module} attr={attribute} value={}", clocks.1);
                }
                let inactive = [fitted(&space, shield_module, ATTR_SHIELD_BONUS).await, fitted(&space, armor_module, ATTR_ARMOR_DAMAGE_AMOUNT).await, fitted(&space, remote_module, ATTR_ARMOR_DAMAGE_AMOUNT).await, fitted(&space, cap_module, ATTR_POWER_TRANSFER_AMOUNT).await];
                let source_health = health(&space, ship).await;
                let target_health = health(&space, target).await;
                let maxima = space.client.world.with(system, move |s| {
                    let source = s.resident_fit(ship, |fit, _| [fit.value(Key::Ship, ATTR_SHIELD_CAPACITY), fit.value(Key::Ship, ATTR_ARMOR_HP)]).expect("source maxima");
                    let armor = s.resident_fit(target, |fit, _| fit.value(Key::Ship, ATTR_ARMOR_HP)).expect("target armor");
                    (source, armor)
                }).await.unwrap();
                let before_cap = crate::services::combat::groups_health::capacitor_snapshot(&db, &space.client.world, system, target).await.unwrap().expect("beneficiary capacitor");
                assert!(source_health[0] + inactive[0].max(1.0) < maxima.0[0] && source_health[1] + inactive[1].max(1.0) < maxima.0[1], "source half health can accept local repair");
                assert!(target_health[1] + inactive[2].max(1.0) < maxima.1 && before_cap.capacity - before_cap.level > inactive[3].max(1.0), "beneficiary half state can accept remote repair and transfer");
                let before_fuel = quantity(&db, stack).await;
                let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
                space.client.world.observe(system, character, ship, destiny).await.unwrap();
                let _ = drain(&mut space.rx);
                let name = effect_name(&sde, effect);
                effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(mode), Value::str(&name), Value::None, Value::Int(0)]).await.ok().expect("one paid mode");
                until(&space.client.world, system, 5_000, || matches!(record(mode), Record::Running)).await;
                let (generation, mode_start) = match combat::state().active_cycles.get(&mode) {
                    Some(ActiveCycle::Running { generation, start, duration_ms, .. }) => { assert!((*duration_ms - 8_000.0).abs() < 1.0, "accepted mode duration"); (generation.clone(), *start) },
                    _ => panic!("running generation"),
                };
                assert_eq!(quantity(&db, stack).await, before_fuel - effective_units(&space, mode).await, "running debits effective fuel");
                assert_eq!(fx_count(&mut events, mode, 1), 1, "one current mode start");
                assert_eq!(godma(&drain(&mut space.rx), mode, i64::from(effect), 1), 1);
                let running = [fitted(&space, shield_module, ATTR_SHIELD_BONUS).await, fitted(&space, armor_module, ATTR_ARMOR_DAMAGE_AMOUNT).await, fitted(&space, remote_module, ATTR_ARMOR_DAMAGE_AMOUNT).await, fitted(&space, cap_module, ATTR_POWER_TRANSFER_AMOUNT).await];
                let consumer_duration = [fitted(&space, shield_module, shield_clock[0].0).await, fitted(&space, armor_module, armor_clock[0].0).await, fitted(&space, remote_module, remote_clock[0].0).await, fitted(&space, cap_module, cap_clock[0].0).await];
                let declared = [&shield_skills, &armor_skills, &armor_skills, &cap_skills];
                for (index, bonus) in declared.into_iter().enumerate() {
                    if bonus.is_empty() { assert_eq!(running[index], inactive[index], "absent skill bonus leaves the ordinary amount"); }
                    else { assert!(running[index] > inactive[index], "a declared family bonus raises the fitted amount"); }
                }
                assert!(running.iter().all(|amount| amount.is_finite() && *amount > 0.0), "every consumer has a positive native amount");
                assert!(source_health[0] + running[0] < maxima.0[0] && source_health[1] + running[1] < maxima.0[1] && target_health[1] + running[2] < maxima.1 && before_cap.level + running[3] < before_cap.capacity, "running amounts fit the published headroom");
                let still = || matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation: current, .. }) if Arc::ptr_eq(current, &generation));
                assert!(still() && space.client.world.with(system, |s| s.now_ms()).await.unwrap() < mode_start + 8_000, "mode generation is inside its accepted clock");
                let started = |notices: &[(String, Vec<Value>)], module: i64, effect: i32| notices.iter().find_map(|(name, args)| {
                    (name == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(module))
                        && args.get(1).and_then(Value::as_int) == Some(i64::from(effect))
                        && args.get(3).and_then(Value::as_int) == Some(1)
                        && args.get(6).and_then(Value::as_int).is_some() && args.get(7).and_then(Value::as_float).is_some())
                        .then(|| (args.get(6).and_then(Value::as_int).expect("start filetime"), args.get(7).and_then(Value::as_float).expect("duration")))
                });
                let mut phase = Vec::new();
                let shield_before = remote_repair_db::condition(&space.client.world, system, ship).await;
                assert!(shield_before.shield_at >= 0 && shield_before.now[0].is_finite() && shield_before.now[0] >= 0.0 && shield_before.stats.max[0].is_finite() && shield_before.stats.max[0] > shield_before.now[0] && shield_before.stats.shield_tau_ms.is_finite() && shield_before.stats.shield_tau_ms >= 0.0 && running[0] > 1.0, "shield fixture requires valid canonical state and an expected fitted amount above the unchanged 1.0 tolerance: effect={effect} mode_type={type_id} module={shield_module} expected={} before={shield_before:?}", running[0]);
                effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(shield_module), Value::str(&effect_name(&sde, EFFECT_SHIELD_BOOSTING)), Value::None, Value::Int(0)]).await.ok().expect("one shield cycle");
                for turn in 0..=20 {
                    phase.extend(drain(&mut space.rx));
                    let (sample, stamp) = space.client.world.with(system, move |s| {
                        (s.combat_condition_snapshot(ship).expect("source shield condition").condition, crate::client::filetime_now())
                    }).await.unwrap();
                    assert!(sample.shield_at >= shield_before.shield_at && sample.stats.max[0] == shield_before.stats.max[0] && sample.stats.shield_tau_ms == shield_before.stats.shield_tau_ms && sample.now[0].is_finite() && sample.now[0] >= 0.0 && sample.now[0] <= sample.stats.max[0], "shield canonical clock or relevant stats changed: effect={effect} mode_type={type_id} module={shield_module} before={shield_before:?} sample={sample:?}");
                    let recharged = crate::services::combat::groups_health::recharge(shield_before.now[0], shield_before.stats.max[0], shield_before.stats.shield_tau_ms, sample.shield_at.saturating_sub(shield_before.shield_at) as f64 / infinity_core::consts::FILETIME_SECOND as f64);
                    assert!(recharged + running[0] < sample.stats.max[0], "shield fixture lacks uncapped canonical headroom: effect={effect} mode_type={type_id} module={shield_module} expected={} recharged={recharged} before={shield_before:?} sample={sample:?}", running[0]);
                    let net = sample.now[0] - recharged;
                    let shield_start = started(&phase, shield_module, EFFECT_SHIELD_BOOSTING);
                    let complete = net > 0.0 && (net - running[0]).abs() < 1.0 && shield_start.is_some();
                    assert!(complete || turn < 20, "local shield did not apply its full expected fitted amount above canonical recharge at start: effect={effect} mode_type={type_id} shield_type={local_shield} module={shield_module} inactive={} expected={} net={net} recharged={recharged} stamp={stamp} phase={shield_start:?} mode_current={} before={shield_before:?} sample={sample:?}", inactive[0], running[0], still());
                    if complete {
                        let (start, span) = shield_start.expect("shield start notice");
                        assert!(span.is_finite() && span > 0.0 && (span - consumer_duration[0]).abs() < 1.0, "shield notice carries the running fitted duration: module={shield_module} span={span} expected={}", consumer_duration[0]);
                        assert!(stamp >= start && stamp < start + crate::client::filetime_ticks_from_ms(span), "full shield amount is observed before its actual start notice deadline: module={shield_module} stamp={stamp} start={start} span={span} net={net}");
                        break;
                    }
                    assert!(space.client.world.after(system, 100).await, "actor stopped");
                }
                assert!(still(), "mode generation survives the shield capture");
                set_locked_target(character, target, true);
                let armor_mark = health(&space, ship).await[1];
                let remote_mark = health(&space, target).await[1];
                let _ = drain(&mut space.rx);
                effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(armor_module), Value::str(&effect_name(&sde, EFFECT_ARMOR_REPAIR)), Value::None, Value::Int(0)]).await.ok().expect("one local armor cycle");
                effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(remote_module), Value::str(&effect_name(&sde, EFFECT_REMOTE_ARMOR_REPAIRER)), Value::Int(target), Value::Int(0)]).await.ok().expect("one remote armor cycle");
                phase.clear();
                until(&space.client.world, system, (consumer_duration[1] + consumer_duration[2]) as u64, || {
                    phase.extend(drain(&mut space.rx));
                    started(&phase, armor_module, EFFECT_ARMOR_REPAIR).is_some() && started(&phase, remote_module, EFFECT_REMOTE_ARMOR_REPAIRER).is_some()
                }).await;
                let (armor_start, armor_span) = started(&phase, armor_module, EFFECT_ARMOR_REPAIR).expect("local armor start notice");
                let (remote_start, remote_span) = started(&phase, remote_module, EFFECT_REMOTE_ARMOR_REPAIRER).expect("remote armor start notice");
                assert!((armor_span - consumer_duration[1]).abs() < 1.0 && (remote_span - consumer_duration[2]).abs() < 1.0, "notices carry the running fitted durations");
                let (stamp, source_armor, target_armor) = space.client.world.with(system, move |s| {
                    let source_armor = s.combat_condition_snapshot(ship).expect("source condition").condition.now[1];
                    let target_armor = s.combat_condition_snapshot(target).expect("target condition").condition.now[1];
                    (crate::client::filetime_now(), source_armor, target_armor)
                }).await.unwrap();
                assert!(stamp < armor_start + (armor_span as i64) * 10_000 && stamp < remote_start + (remote_span as i64) * 10_000, "the sample is before both notice deadlines");
                assert_eq!(source_armor, armor_mark, "local armor is unchanged in the same turn as the pre-deadline stamp");
                assert_eq!(target_armor, remote_mark, "remote armor is unchanged in the same turn as the pre-deadline stamp");
                until(&space.client.world, system, (armor_span + remote_span) as u64, || crate::client::filetime_now() >= armor_start + (armor_span as i64) * 10_000 && crate::client::filetime_now() >= remote_start + (remote_span as i64) * 10_000).await;
                let armor_after = wait_health_above(&space.client.world, system, ship, 1, armor_mark, 40).await;
                let remote_after = wait_health_above(&space.client.world, system, target, 1, remote_mark, 40).await;
                assert!((armor_after - armor_mark - running[1]).abs() < 1.0, "local armor applies its captured amount after its notice deadline");
                assert!((remote_after - remote_mark - running[2]).abs() < 1.0, "remote armor lands its captured amount after its notice deadline");
                assert_eq!(health(&space, ship).await[1], armor_after, "outgoing repair does not add a second amount on the source");
                let cap_before = crate::services::combat::groups_health::capacitor_snapshot(&db, &space.client.world, system, target).await.unwrap().expect("capacitor before transfer");
                let cap_opened = crate::client::filetime_now();
                let tau = space.client.world.with(system, move |s| s.resident_fit(target, |fit, _| fit.value(Key::Ship, ATTR_RECHARGE_RATE))).await.unwrap().expect("beneficiary tau");
                assert!(tau >= 1_000_000_000.0, "the published recharge tau stays long enough that passive recharge cannot imitate one transfer");
                effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(cap_module), Value::str(&effect_name(&sde, EFFECT_REMOTE_CAPACITOR_TRANSMITTER)), Value::Int(target), Value::Int(0)]).await.ok().expect("one capacitor transfer");
                let cap_limit = cap_opened + (consumer_duration[3] as i64) * 10_000 + 2_000 * 10_000;
                let (base_level, base_capacity) = (cap_before.level, cap_before.capacity);
                let mut matched = false;
                let mut net = 0.0;
                while crate::client::filetime_now() < cap_limit {
                    assert!(space.client.world.after(system, 100).await, "actor stopped during the transfer");
                    let sample = crate::services::combat::groups_health::capacitor_snapshot(&db, &space.client.world, system, target).await.unwrap().expect("capacitor during transfer");
                    let elapsed = (crate::client::filetime_now() - cap_opened).max(0) as f64 / 10_000_000.0;
                    let regenerated = crate::services::combat::groups_health::recharge(base_level, base_capacity, tau, elapsed) - base_level;
                    net = sample.level - base_level - regenerated.max(0.0);
                    if (net - running[3]).abs() < 1.0 && sample.level < sample.capacity { matched = true; break; }
                }
                assert!(matched, "the poll observed the captured transfer above recharge");
                assert!((net - running[3]).abs() < 1.0, "accepted transfer equals the captured amount above recharge");
                assert!(still() && space.client.world.with(system, |s| s.now_ms()).await.unwrap() < mode_start + 8_000, "mode generation survives inside its accepted clock");
            }).await;
            set_locked_target(character, target, false);
            crate::services::tests::fixture::cleanup_ship(&db, Some(&space.client.world), target_fixture).await.unwrap();
            if let Err(panic) = nested { std::panic::resume_unwind(panic); }
        }).await;
        teardown(&db, space).await;
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn triage_resistance_and_owner_drones_follow_exact_selectors() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (modified, skill) = drone_modifier(&sde, EFFECT_TRIAGE);
    let drone = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_DRONE)
        && ty.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|used| used > 0.0)
        && ty.effects().iter().any(|linked| crate::services::effects::registry::resolve(&sde, linked.effect_id).is_some_and(|effect| effect.kind == crate::services::effects::EffectKind::Turret))
        && ty.attribute(modified).is_some_and(|amount| amount > 0.0)
        && crate::services::skills::requirements::requirements(&sde, ty.type_id).any(|(required, _)| required == skill)).map(|ty| ty.type_id).expect("skill-matched turret drone");
    let remote = |family: i32, amount: i32| sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
        && ty.effects().iter().any(|linked| linked.effect_id == family && linked.is_default) && ty.attribute(amount).is_some_and(|value| value > 0.0)
        && crate::services::effects::fitting::rack(&sde.types, ty.type_id).is_some()).map(|ty| ty.type_id).expect("remote module");
    let (armor_type, cap_type) = (remote(EFFECT_REMOTE_ARMOR_REPAIRER, ATTR_ARMOR_DAMAGE_AMOUNT), remote(EFFECT_REMOTE_CAPACITOR_TRANSMITTER, ATTR_POWER_TRANSFER_AMOUNT));
    let clock = |family: i32| sde.dogma.effect(family).and_then(|effect| effect.duration_attribute_id).expect("duration");
    let (armor_clock, cap_clock) = ([(clock(EFFECT_REMOTE_ARMOR_REPAIRER), DURATION_MS)], [(clock(EFFECT_REMOTE_CAPACITOR_TRANSMITTER), DURATION_MS)]);
    let ecm_type = sde.types.iter().find(|ty| ty.published == Some(true) && ty.effects().iter().any(|effect| effect.effect_id == EFFECT_REMOTE_ECM_FALLOFF)).expect("ecm").type_id;
    let damp_type = sde.types.iter().find(|ty| ty.published == Some(true) && ty.effects().iter().any(|linked| linked.effect_id == infinity_core::consts::dogma::EFFECT_REMOTE_SENSOR_DAMP_FALLOFF && linked.is_default) && sde.dogma.effect(infinity_core::consts::dogma::EFFECT_REMOTE_SENSOR_DAMP_FALLOFF).is_some_and(|row| row.modifiers().iter().any(|modifier| modifier.domain == "targetID" && modifier.modified == ATTR_MAX_TARGET_RANGE)) && crate::services::effects::fitting::rack(&sde.types, ty.type_id).is_some()).map(|ty| ty.type_id).expect("published damp");
    let mode_duration = |effect: i32| sde.dogma.effect(effect).and_then(|row| row.duration_attribute_id).expect("mode duration");
    let placed = sde.map.systems().find(|row| !crate::services::crimewatch::highsec_system(&sde, row.solar_system_id)
        && sde.map.celestials(row.solar_system_id).any(|c| c.x.is_some() && c.y.is_some() && c.z.is_some())).expect("eligible system").solar_system_id;
    for effect in [EFFECT_TRIAGE, EFFECT_STELLIAN_TRIAGE] {
        let module = published_links(&sde, effect).into_iter().find(|type_id| {
            crate::services::skills::requirements::requirements(&sde, *type_id).any(|(_, level)| level == 1)
        }).unwrap_or(published_links(&sde, effect)[0]);
        let fuel = ShipItemSpec { type_id: fuel_type(&sde, module), flag_id: FLAG_CARGO, quantity: raw_units(&sde, module), singleton: false, online: false, attributes: &[] };
        let drone_spec = ShipItemSpec { type_id: drone, flag_id: FLAG_DRONE_BAY, quantity: 1, singleton: true, online: false, attributes: &[] };
        let initial = (effect == EFFECT_STELLIAN_TRIAGE).then_some(placed);
        let mut space = in_space(&db, &sde, module, effect, &[(mode_duration(effect), 8_000.0)], &[fuel, drone_spec], initial).await;
        let result = crate::tests::catch_scenario(async {
            let (scene, mode, ship, character, stack) = (space.system, space.module, space.fixture.ship, space.fixture.character, space.fixture.items[1]);
            let drone_id = space.fixture.items[2];
            let hull = space.client.world.ball(scene, ship).await.expect("receiver").type_id;
            let sender_specs = [module_spec(&sde, armor_type, 0, &armor_clock), module_spec(&sde, cap_type, 1, &cap_clock), module_spec(&sde, ecm_type, 2, &[]), module_spec(&sde, damp_type, 3, &[])];
            let sender = create_ship(&db, hull, ShipLocation::Space { system: scene }, &sender_specs).await.unwrap();
            let target = create_ship(&db, hull, ShipLocation::Space { system: scene }, &[]).await.unwrap();
            let nested = crate::tests::catch_scenario(async {
                let pos = space.client.world.ball(scene, ship).await.expect("receiver").pos;
                for (index, fixture) in [&sender, &target].iter().enumerate() {
                    let x = pos[0] + 1_000.0 * (index as f64 + 1.0);
                    let mut ball = effects_fixture::ball(fixture.ship, hull, fixture.character, Some(fixture.character), x);
                    ball.pos = [x, pos[1], pos[2]];
                    ball.old_pos = ball.pos;
                    effects_fixture::admit(&db, &space.client.world, scene, ball).await;
                }
                for (item, attribute, value) in [(ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0), (ship, ATTR_DRONE_CAPACITY, 1_000_000.0), (ship, ATTR_MAX_ACTIVE_DRONES, 5.0), (ship, ATTR_ARMOR_HP, 100_000.0), (ship, ATTR_CAPACITOR_CAPACITY, 100_000.0), (ship, ATTR_RECHARGE_RATE, 1_000_000_000.0), (ship, ATTR_MAX_TARGET_RANGE, 10_000.0), (ship, ATTR_SCAN_RESOLUTION, 1_000_000.0), (target.ship, ATTR_ARMOR_HP, 100_000.0), (target.ship, ATTR_SIGNATURE_RADIUS, 1_000_000.0), (sender.ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0), (sender.ship, ATTR_MAX_LOCKED_TARGETS, 5.0), (sender.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0), (sender.ship, ATTR_CPU_OUTPUT, 1_000_000_000.0), (sender.ship, ATTR_POWER_OUTPUT, 1_000_000_000.0), (sender.ship, ATTR_CAPACITOR_CAPACITY, 1_000_000_000.0)] {
                    item_dogma::set_override(&db, item, attribute, value).await.unwrap();
                }
                for (owner, item) in [(character, ship), (sender.character, sender.ship), (target.character, target.ship)] {
                    effects_fixture::publish_fit(&db, &space.client.world, scene, owner, item).await;
                }
                let receiver_fit = Fit::load_in_space(&db, &space.client.world, scene, character, ship).await.unwrap();
                remote_repair_db::install_half(&space.client.world, scene, ship, receiver_fit).await;
                let half = health(&space, ship).await;
                assert!(half[1] > 0.0 && half[1] < ship_value(&space, ATTR_ARMOR_HP).await, "receiver armor starts below its maximum");
                let (armor_module, cap_module, ecm_module, damp_module) = (sender.items[0], sender.items[1], sender.items[2], sender.items[3]);
                let armor_range = crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_ARMOR_REPAIRER).expect("armor").attribute(crate::services::effects::registry::EffectAttribute::Range).expect("armor range");
                let cap_range = crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_CAPACITOR_TRANSMITTER).expect("transfer").attribute(crate::services::effects::registry::EffectAttribute::Range).expect("transfer range");
                let ecm_range = crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_ECM_FALLOFF).expect("ecm").attribute(crate::services::effects::registry::EffectAttribute::Range).expect("ecm range");
                for (module, attribute, value) in [(armor_module, armor_clock[0].0, DURATION_MS), (cap_module, cap_clock[0].0, DURATION_MS), (armor_module, ATTR_ARMOR_DAMAGE_AMOUNT, 1_000.0), (cap_module, ATTR_POWER_TRANSFER_AMOUNT, 1_000.0), (armor_module, armor_range, 1_000_000.0), (cap_module, cap_range, 1_000_000.0), (ecm_module, ecm_range, 1_000_000.0)] {
                    item_dogma::set_override(&db, module, attribute, value).await.unwrap();
                }
                let damp_range = crate::services::effects::registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_REMOTE_SENSOR_DAMP_FALLOFF).expect("damp").attribute(crate::services::effects::registry::EffectAttribute::Range).expect("damp range");
                item_dogma::set_override(&db, damp_module, damp_range, 1_000_000.0).await.unwrap();
                for attribute in crate::services::effects::ecm::JAM_STRENGTH { item_dogma::set_override(&db, ecm_module, attribute, 40.0).await.unwrap(); }
                item_dogma::set_override(&db, ship, ATTR_SCAN_GRAVIMETRIC_STRENGTH, 100.0).await.unwrap();
                effects_fixture::publish_fit(&db, &space.client.world, scene, sender.character, sender.ship).await;
                item_dogma::set_override(&db, drone_id, infinity_core::consts::dogma::ATTR_MAX_VELOCITY, 0.0).await.unwrap();
                item_dogma::set_override(&db, drone_id, ATTR_MAX_RANGE, 1_000_000.0).await.unwrap();
                effects_fixture::publish_fit(&db, &space.client.world, scene, character, ship).await;
                let login = crate::handshake::Login { user_id: sender.account, user_name: "sender".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
                let (_listener, _peer, mut sender_client) = crate::services::tests::connected_client(db.clone(), sde.clone(), space.client.world.clone(), space.client.cfg.clone(), login).await;
                sender_client.session.apply(vec![("userid", Value::Int(sender.account)), ("charid", Value::Int(sender.character)), ("shipid", Value::Int(sender.ship)), ("solarsystemid", Value::Int(scene)), ("solarsystemid2", Value::Int(scene)), ("role", Value::Int(0))]);
                sender_client.login.user_id = sender.account;
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                sender_client.notify_tx = tx.clone();
                sender_client.world.register_pilot(sender.character, tx);
                gm::apply(&mut sender_client, sender.character, Change::Set([armor_type, cap_type, ecm_type, damp_type, hull].into_iter().flat_map(|type_id| crate::services::skills::requirements::requirements(&sde, type_id)).map(|(skill, _)| (skill, 5)).collect::<std::collections::BTreeMap<_, _>>().into_iter().collect())).await.expect("sender required skills");
                let cx = combat::ctx(&sender_client).expect("sender scene");
                assert!(combat::targeting_charges::add_target_ctx(&cx, ship, false).await.is_ok());
                until(&space.client.world, scene, 5_000, || combat::targeting_charges::targets_of(sender.character).contains(&ship)).await;
                let neutral_ecm = ship_value(&space, crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_ECM_FALLOFF).expect("ecm").attribute(crate::services::effects::registry::EffectAttribute::Resistance).expect("ecm resistance")).await;
                let neutral_range = ship_value(&space, ATTR_MAX_TARGET_RANGE).await;
                let early_inputs = damp_inputs(&space, sender.ship, damp_module).await;
                let damp_resistance_id = early_inputs.resistance_attribute.expect("captured resistance");
                let neutral_damp = ship_value(&space, damp_resistance_id).await;
                let kwargs = Value::Dict(vec![]);
                crate::services::drones::inventory::launch::launch(&mut space.client, &crate::services::Call { service: "ship", method: "LaunchDrones", args: &[Value::list([Value::tuple([Value::Int(drone_id), Value::Int(1)])]), Value::None, Value::Bool(false)], kwargs: &kwargs, bound: true, bind_params: Some(&Value::list([Value::Int(scene), Value::Int(GROUP_SOLAR_SYSTEM)])) }).await.expect("launch");
                let neutral_effect = crate::services::effects::registry::resolve(&sde, space.client.world.with(scene, move |system| system.drone(drone_id).expect("launched").spec.effect_id.expect("turret effect")).await.unwrap()).expect("neutral effect");
                let neutral_captured = space.client.world.with(scene, {
                    let neutral_effect = neutral_effect.clone();
                    move |system| system.with_combat_fit(drone_id, |fit, key, _| crate::services::effects::parameters::capture_source(fit, key, &neutral_effect, 0.0))
                }).await.unwrap().expect("neutral fit").expect("neutral capture");
                let crate::services::effects::parameters::SourceInputs::Turret(neutral_turret) = neutral_captured else { panic!("neutral turret"); };
                let neutral_drone = resident_drone(&space, drone_id, modified).await;
                let neutral_state = space.client.world.with(scene, move |system| {
                    let ball = system.ball(drone_id).expect("launched ball");
                    let record = system.drone(drone_id).expect("launched record");
                    (ball.max_vel, ball.vel, record.order, record.controller, record.owner)
                }).await.unwrap();
                assert!(neutral_turret.raw.iter().all(|amount| amount.is_finite() && *amount >= 0.0) && neutral_turret.raw.iter().any(|amount| *amount > 0.0) && neutral_drone > 0.0, "neutral raw");
                assert!(neutral_state.0 == 0.0 && neutral_state.1 == [0.0; 3] && neutral_state.2 == crate::space::drone::DroneOrder::Idle && neutral_state.3 == Some(ship) && neutral_state.4 == character, "idle launched drone");
                let before_fuel = quantity(&db, stack).await;
                activate(&mut space, &effect_name(&sde, effect), mode).await;
                until(&space.client.world, scene, 5_000, || matches!(record(mode), Record::Running)).await;
                assert_eq!(quantity(&db, stack).await, before_fuel - effective_units(&space, mode).await);
                let running_effect = crate::services::effects::registry::resolve(&sde, space.client.world.with(scene, move |system| system.drone(drone_id).expect("running").spec.effect_id.expect("turret effect")).await.unwrap()).expect("running effect");
                let running_captured = space.client.world.with(scene, {
                    let running_effect = running_effect.clone();
                    move |system| system.with_combat_fit(drone_id, |fit, key, _| crate::services::effects::parameters::capture_source(fit, key, &running_effect, 0.0))
                }).await.unwrap().expect("running fit").expect("running capture");
                let crate::services::effects::parameters::SourceInputs::Turret(running_turret) = running_captured else { panic!("running turret"); };
                let running_drone = resident_drone(&space, drone_id, modified).await;
                let running_state = space.client.world.with(scene, move |system| system.ball(drone_id).expect("running ball").max_vel).await.unwrap();
                assert!(running_state == 0.0, "running max velocity stays zero");
                assert!(if effect == EFFECT_TRIAGE { running_drone < neutral_drone && running_turret.raw.iter().all(|amount| *amount == 0.0) } else { running_drone == neutral_drone && running_turret.raw == neutral_turret.raw && running_turret.raw.iter().any(|amount| *amount > 0.0) }, "owner raw");
                let running_range = ship_value(&space, ATTR_MAX_TARGET_RANGE).await;
                let running_damp = ship_value(&space, damp_resistance_id).await;
                assert!(neutral_damp.is_finite() && running_damp.is_finite() && running_damp > 0.0 && running_damp < neutral_damp && neutral_range.is_finite() && neutral_range > 0.0, "mode damp resistance is strict");
                let mode_generation = match combat::state().active_cycles.get(&mode) { Some(ActiveCycle::Running { generation, .. }) => generation.clone(), _ => panic!("receiver generation") };
                let armor_effect = crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_ARMOR_REPAIRER).expect("armor effect");
                let cap_effect = crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_CAPACITOR_TRANSMITTER).expect("transfer effect");
                let resistance_attr = crate::services::effects::registry::EffectAttribute::Resistance;
                let armor_resistance = ship_value(&space, armor_effect.attribute(resistance_attr).expect("armor resistance")).await;
                let cap_resistance = ship_value(&space, cap_effect.attribute(resistance_attr).expect("transfer resistance")).await;
                for (item, attribute, factor) in [(armor_module, ATTR_ARMOR_DAMAGE_AMOUNT, armor_resistance), (cap_module, ATTR_POWER_TRANSFER_AMOUNT, cap_resistance)] {
                    assert!(factor.is_finite() && factor > 0.0 && factor < 1.0, "incoming numeric resistance {factor}");
                    let amount = 1_000.0 / factor;
                    assert!(amount.is_finite() && amount > 0.0, "finite sender amount");
                    item_dogma::set_override(&db, item, attribute, amount).await.unwrap();
                }
                effects_fixture::publish_fit(&db, &space.client.world, scene, sender.character, sender.ship).await;
                let (armor_operand, cap_operand) = space.client.world.with(scene, move |system| system.resident_fit(sender.ship, |fit, _| (fit.value(Key::Module(armor_module), ATTR_ARMOR_DAMAGE_AMOUNT), fit.value(Key::Module(cap_module), ATTR_POWER_TRANSFER_AMOUNT)))).await.unwrap().expect("unmoded sender operands");
                let expected_armor = armor_operand * armor_resistance;
                let expected_cap = cap_operand * cap_resistance;
                let before_armor = health(&space, ship).await[1];
                let armor_max = ship_value(&space, ATTR_ARMOR_HP).await;
                assert!(expected_armor.is_finite() && expected_armor > 1.0 && before_armor > 0.0 && before_armor + expected_armor < armor_max, "uncapped incoming armor");
                effects_fixture::dogma_call(&mut sender_client, "Activate", &[Value::Int(armor_module), Value::str(&effect_name(&sde, EFFECT_REMOTE_ARMOR_REPAIRER)), Value::Int(ship), Value::Int(0)]).await.expect("one incoming armor cycle");
                let repaired = wait_health_above(&space.client.world, scene, ship, 1, before_armor, (DURATION_MS as usize / 100) + 20).await;
                assert!((repaired - before_armor - expected_armor).abs() < expected_armor * 0.01, "incoming armor actual={} expected={expected_armor}", repaired - before_armor);
                let full_cap = crate::services::combat::groups_health::capacitor_snapshot(&db, &space.client.world, scene, ship).await.unwrap().expect("receiver capacitor");
                let cap_tau = ship_value(&space, ATTR_RECHARGE_RATE).await;
                assert!(crate::services::combat::groups_health::drain_capacitor(&combat::ctx(&space.client).expect("receiver"), full_cap.capacity, cap_tau, expected_cap * 2.0), "canonical headroom");
                let before_cap = crate::services::combat::groups_health::capacitor_snapshot(&db, &space.client.world, scene, ship).await.unwrap().expect("receiver capacitor");
                assert!(expected_cap.is_finite() && expected_cap > 1.0 && before_cap.level + expected_cap < before_cap.capacity, "uncapped incoming capacitor; before={} expected={expected_cap} capacity={}", before_cap.level, before_cap.capacity);
                let _ = drain(&mut space.rx);
                effects_fixture::dogma_call(&mut sender_client, "Activate", &[Value::Int(cap_module), Value::str(&effect_name(&sde, EFFECT_REMOTE_CAPACITOR_TRANSMITTER)), Value::Int(ship), Value::Int(0)]).await.expect("one incoming capacitor cycle");
                let mut application = None;
                for _ in 0..20 {
                    for (name, args) in drain(&mut space.rx) {
                        if name != "OnModuleAttributeChanges" { continue; }
                        let [Value::List(changes)] = args.as_slice() else { continue; };
                        for change in changes {
                            let Value::Tuple(row) = change else { continue; };
                            if row.len() != 8 || row[1] != Value::Int(character) || row[2] != Value::Int(ship) || row[3] != Value::Int(i64::from(ATTR_CHARGE)) { continue; }
                            let (Some(stamp), Some(new), Some(old)) = (row[4].as_int(), row[5].as_float(), row[6].as_float()) else { panic!("canonical capacitor event"); };
                            assert!(application.is_none(), "one native receiver injection");
                            application = Some((stamp, old, new));
                        }
                    }
                    if application.is_some() { break; }
                    assert!(space.client.world.after(scene, 100).await, "actor stopped during incoming capacitor");
                }
                let (applied_at, old, new) = application.expect("native incoming capacitor application, not recharge");
                assert!(applied_at > 0 && old.is_finite() && new.is_finite() && old >= before_cap.level && old + expected_cap < before_cap.capacity, "application-time canonical headroom");
                assert!((new - old - expected_cap).abs() < expected_cap * 0.01, "incoming capacitor actual={} expected={expected_cap}", new - old);
                let after_cap = crate::services::combat::groups_health::capacitor_snapshot(&db, &space.client.world, scene, ship).await.unwrap().expect("applied capacitor");
                assert!(after_cap.capacity == before_cap.capacity && after_cap.level >= new && after_cap.level < after_cap.capacity, "canonical receiver retains the uncapped injection");
                assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { .. })), "receiver remains in its real mode");
                let inputs = damp_inputs(&space, sender.ship, damp_module).await;
                assert_eq!(inputs.resistance_attribute, Some(damp_resistance_id), "fresh payload keeps the early resistance");
                let values: Vec<(i32, f64)> = sde.dogma.effect(infinity_core::consts::dogma::EFFECT_REMOTE_SENSOR_DAMP_FALLOFF).expect("damp row").modifiers().iter().filter(|row| row.domain == "targetID").map(|row| {
                    let (_, operand, _) = inputs.module_values.iter().find(|(attribute, _, _)| *attribute == row.modifying).expect("captured source operand");
                    (row.modifying, *operand)
                }).collect();
                assert!(values.iter().any(|(attribute, operand)| sde.dogma.effect(infinity_core::consts::dogma::EFFECT_REMOTE_SENSOR_DAMP_FALLOFF).expect("damp row").modifiers().iter().any(|row| row.domain == "targetID" && row.modified == ATTR_MAX_TARGET_RANGE && row.modifying == *attribute) && operand.is_finite()), "captured finite range operand");
                assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation, .. }) if std::sync::Arc::ptr_eq(generation, &mode_generation)), "mode generation before projection");
                let resistance_attribute = inputs.resistance_attribute;
                let projected_range = space.client.world.with(scene, move |system| {
                    system.upsert_projection(ship, crate::services::effects::projection::Projection {
                        source_ship: sender.ship,
                        source_module: damp_module,
                        source_type: damp_type,
                        effect_id: infinity_core::consts::dogma::EFFECT_REMOTE_SENSOR_DAMP_FALLOFF,
                        kind: crate::services::effects::EffectKind::Projected { jamming: crate::services::effects::JammingType::Damp },
                        until_ms: i64::MAX,
                        values,
                        charge_values: vec![],
                        charge_type: None,
                        resistance_attribute,
                        scramble: None,
                    });
                    system.resident_fit(ship, |fit, _| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE))
                }).await.unwrap().expect("projected receiver range");
                assert!(projected_range.is_finite() && projected_range > 0.0 && projected_range < running_range, "strict projected interval");
                let (receiver_radius, origin) = space.client.world.with(scene, move |system| {
                    let receiver = system.ball(ship).expect("receiver");
                    (receiver.radius, system.sample_position(ship).expect("receiver position"))
                }).await.unwrap();
                let range_target = create_ship(&db, hull, ShipLocation::Space { system: scene }, &[]).await.unwrap();
                let range_result = crate::tests::catch_scenario(async {
                    let gap = (projected_range + running_range) / 2.0;
                    let mut ball = effects_fixture::ball(range_target.ship, hull, range_target.character, Some(range_target.character), origin[0]);
                    let x = origin[0] + f64::from(receiver_radius) + f64::from(ball.radius) + gap;
                    ball.pos = [x, origin[1], origin[2]];
                    ball.old_pos = ball.pos;
                    effects_fixture::admit(&db, &space.client.world, scene, ball).await;
                    let (surface, fresh_range) = space.client.world.with(scene, move |system| {
                        let (from, to) = system.sample_position(ship).zip(system.sample_position(range_target.ship)).expect("interval positions");
                        let source = system.ball(ship).expect("receiver");
                        let victim = system.ball(range_target.ship).expect("interval target");
                        let surface = (crate::space::dist(from, to) - f64::from(source.radius) - f64::from(victim.radius)).max(0.0);
                        let fresh_range = system.resident_fit(ship, |fit, _| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE)).expect("fresh range");
                        (surface, fresh_range)
                    }).await.unwrap();
                    assert!(space.client.world.with(scene, move |system| system.visible_to(character, range_target.ship)).await.unwrap() && surface > fresh_range && surface < running_range, "visible strict interval");
                    let receiver_ctx = combat::ctx(&space.client).expect("receiver scene");
                    let refused = combat::targeting_charges::add_target_ctx(&receiver_ctx, range_target.ship, false).await;
                    let expected = Value::unicode(messages::target_lock_range(crate::space::metres_to_km(surface), crate::space::metres_to_km(fresh_range)));
                    assert!(matches!(refused, Err(CallError::User { ref msg, ref dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict.iter().any(|(key, value)| key == "notify" && value == &expected)), "canonical projected range refusal");
                    space.client.world.with(scene, move |system| system.remove_projection(damp_module)).await.unwrap();
                    assert!(combat::targeting_charges::add_target_ctx(&receiver_ctx, range_target.ship, false).await.is_ok(), "range admitted after removal");
                    until(&space.client.world, scene, 5_000, || combat::targeting_charges::targets_of(character).contains(&range_target.ship)).await;
                    assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation, .. }) if std::sync::Arc::ptr_eq(generation, &mode_generation)), "mode generation after lock");
                }).await;
                cleanup_ship(&db, Some(&*space.client.world), range_target).await.unwrap();
                if let Err(panic) = range_result { std::panic::resume_unwind(panic); }
                let running_ecm = ship_value(&space, crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_ECM_FALLOFF).expect("ecm").attribute(crate::services::effects::registry::EffectAttribute::Resistance).expect("running ecm resistance")).await;
                assert!(running_ecm.is_finite() && running_ecm > 0.0 && running_ecm < neutral_ecm, "running ecm resistance is strict");
                let effect_row = crate::services::effects::registry::resolve(&sde, EFFECT_REMOTE_ECM_FALLOFF).expect("ecm");
                let sensors = space.client.world.with(scene, move |system| system.resident_fit(ship, |fit, _| [ATTR_SCAN_GRAVIMETRIC_STRENGTH, ATTR_SCAN_LADAR_STRENGTH, ATTR_SCAN_MAGNETOMETRIC_STRENGTH, ATTR_SCAN_RADAR_STRENGTH].map(|attribute| fit.value(Key::Ship, attribute)))).await.unwrap().expect("receiver sensors");
                space.client.world.with(scene, { let sde = sde.clone(); let effect_row = effect_row.clone(); move |system| crate::services::effects::ecm::tests::module_admission::assert_triage_receiver_ecm(system, &sde, sender.ship, ecm_module, ship, &effect_row, running_ecm, sensors) }).await.unwrap();
                assert!(!crate::services::combat::jamming::is_jammed(ship, space.client.world.with(scene, |system| system.now_ms()).await.unwrap()), "helper cleared the admitted jam");
                assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation, .. }) if std::sync::Arc::ptr_eq(generation, &mode_generation)), "mode generation after ecm");
                let _ = drain(&mut space.rx);
                let before_condition = remote_repair_db::condition(&space.client.world, scene, target.ship).await;
                let before_target = before_condition.now;
                assert!(before_target.iter().all(|amount| amount.is_finite()), "canonical target health");
                assert_eq!(before_condition.now, before_condition.stats.max, "target starts full");
                let owner_ctx = combat::ctx(&space.client).expect("owner scene");
                assert!(combat::targeting_charges::add_target_ctx(&owner_ctx, target.ship, false).await.is_ok(), "owner admits the drone target");
                until(&space.client.world, scene, 5_000, || combat::targeting_charges::targets_of(character).contains(&target.ship)).await;
                crate::services::drones::dispatch(&mut space.client, &crate::services::Call { service: "entity", method: "CmdEngage", args: &[Value::list([Value::Int(drone_id)]), Value::Int(target.ship)], kwargs: &kwargs, bound: true, bind_params: Some(&Value::Int(scene)) }).await.expect("entity dispatch").expect("engage");
                let engaged = space.client.world.with(scene, move |system| system.drone(drone_id).expect("engaged").generation).await.unwrap();
                let mut first = None;
                for _ in 0..40 {
                    assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation, .. }) if std::sync::Arc::ptr_eq(generation, &mode_generation)), "mode remains through the hit");
                    for (name, args) in drain(&mut space.rx) {
                        if name != "OnDamageMessage" { continue; }
                        let [Value::Dict(fields)] = args.as_slice() else { continue; };
                        let field = |key: &str| fields.iter().find(|(name, _)| matches!(name, Value::Bytes(bytes) if bytes.as_slice() == key.as_bytes())).map(|(_, value)| value);
                        if field("target") != Some(&Value::Int(target.ship)) || field("weapon") != Some(&Value::Int(i64::from(drone))) { continue; }
                        let quality = field("hitQuality").and_then(Value::as_int).expect("quality");
                        let damage = field("damage").and_then(Value::as_float).expect("damage");
                        assert!(first.is_none(), "first matching hit");
                        first = Some((quality, damage));
                    }
                    if first.is_some() { break; }
                    assert!(space.client.world.after(scene, 100).await, "actor stopped");
                }
                let (quality, damage) = first.expect("first native hit");
                let sde_for_hit = sde.clone();
                let applied = space.client.world.with(scene, move |system| {
                    let drone_ball = system.sampled_ball(drone_id).expect("drone");
                    let victim = system.sampled_ball(target.ship).expect("target");
                    let chance = crate::services::effects::weapons::turret_chance(&drone_ball, &victim, running_turret.parameters.0, running_turret.parameters.1, running_turret.parameters.2, running_turret.parameters.3, crate::services::effects::weapons::target_signature(&sde_for_hit, &victim));
                    let record = system.drone(drone_id).expect("record");
                    (drone_ball.vel, victim.vel, chance, record.generation, record.owner, record.controller, record.target)
                }).await.unwrap();
                let after_target = health(&space, target.ship).await;
                assert!(applied.0 == [0.0; 3] && applied.1 == [0.0; 3] && applied.2 == 1.0 && applied.3 == engaged && applied.4 == character && applied.5 == Some(ship) && applied.6 == Some(target.ship), "stationary application");
                assert!(quality > 0, "positive delivered quality");
                if effect == EFFECT_TRIAGE { assert!(damage == 0.0 && after_target == before_target, "ordinary zero"); }
                else { assert!(damage > 0.0 && after_target.iter().sum::<f64>() < before_target.iter().sum(), "Stellian damage"); }
                crate::services::drones::dispatch(&mut space.client, &crate::services::Call { service: "entity", method: "CmdReturnHome", args: &[Value::list([Value::Int(drone_id)])], kwargs: &kwargs, bound: true, bind_params: Some(&Value::Int(scene)) }).await.expect("entity dispatch").expect("return");
                assert!(space.client.world.with(scene, move |system| system.drone(drone_id).is_some_and(|drone| drone.generation != engaged && matches!(drone.order, crate::space::drone::DroneOrder::ReturnHome | crate::space::drone::DroneOrder::Idle) && drone.order != crate::space::drone::DroneOrder::Engage(target.ship))).await.unwrap(), "return cancels the engagement");
            }).await;
            cleanup_ship(&db, Some(&*space.client.world), sender).await.unwrap();
            cleanup_ship(&db, Some(&*space.client.world), target).await.unwrap();
            if let Err(panic) = nested { std::panic::resume_unwind(panic); }
        }).await;
        teardown(&db, space).await;
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn triage_lock_limits_and_stellian_security_use_current_fit() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let ordinary = published_links(&sde, EFFECT_TRIAGE).into_iter().find(|type_id| {
        crate::services::skills::requirements::requirements(&sde, *type_id).any(|(_, level)| level == 1)
    }).expect("ordinary triage requiring level 1");
    let stellian = published_links(&sde, EFFECT_STELLIAN_TRIAGE)[0];
    let scene_of = |high: bool| sde.map.systems().find(|row| crate::services::crimewatch::highsec_system(&sde, row.solar_system_id) == high
        && sde.map.celestials(row.solar_system_id).any(|c| c.x.is_some() && c.y.is_some() && c.z.is_some())).expect("positioned system").solar_system_id;
    let (placed, high) = (scene_of(false), scene_of(true));
    let duration = |effect: i32| sde.dogma.effect(effect).and_then(|row| row.duration_attribute_id).expect("duration");
    let skill = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SKILL) && ty.effects().iter().any(|linked| {
        sde.dogma.effect(linked.effect_id).is_some_and(|row| row.effect_category == Some(0)
            && row.modifiers().iter().any(|m| m.domain == "charID" && m.modified == ATTR_MAX_LOCKED_TARGETS))
    })).map(|ty| ty.type_id).expect("character lock skill");
    for (effect, type_id, system) in [(EFFECT_TRIAGE, ordinary, None), (EFFECT_STELLIAN_TRIAGE, stellian, Some(placed))] {
        let fuel = ShipItemSpec { type_id: fuel_type(&sde, type_id), flag_id: FLAG_CARGO, quantity: raw_units(&sde, type_id), singleton: false, online: false, attributes: &[] };
        let mut space = in_space(&db, &sde, type_id, effect, &[(duration(effect), 8_000.0)], &[fuel], system).await;
        let result = crate::tests::catch_scenario(async {
            let (scene, mode, ship, character, stack) = (space.system, space.module, space.fixture.ship, space.fixture.character, space.fixture.items[1]);
            let hull = space.client.world.ball(scene, ship).await.expect("source").type_id;
            let signature = sde.types.attribute(hull, ATTR_SIGNATURE_RADIUS).expect("hull signature");
            assert!(signature.is_finite() && signature > 0.0, "selected hull signature={signature}");
            let targets = [create_ship(&db, hull, ShipLocation::Space { system: scene }, &[]).await.unwrap(), create_ship(&db, hull, ShipLocation::Space { system: scene }, &[]).await.unwrap()];
            let nested = crate::tests::catch_scenario(async {
                let pos = space.client.world.ball(scene, ship).await.expect("source").pos;
                for (index, target) in targets.iter().enumerate() {
                    let x = pos[0] + 1_000.0 * (index as f64 + 1.0);
                    let mut ball = effects_fixture::ball(target.ship, hull, target.character, Some(target.character), x);
                    ball.pos = [x, pos[1], pos[2]];
                    ball.old_pos = ball.pos;
                    effects_fixture::admit(&db, &space.client.world, scene, ball).await;
                }
                gm::apply(&mut space.client, character, Change::Set(vec![(skill, 5)])).await.expect("pilot skill");
                for (attribute, value) in [(ATTR_MAX_LOCKED_TARGETS, 1.0), (ATTR_MAX_TARGET_RANGE, 1_000_000.0), (ATTR_SCAN_RESOLUTION, 1_000_000.0)] {
                    item_dogma::set_override(&db, ship, attribute, value).await.unwrap();
                }
                effects_fixture::publish_fit(&db, &space.client.world, scene, character, ship).await;
                let (ship_limit, pilot_limit) = space.client.world.with(scene, move |s| s.with_combat_fit(ship, |fit, _, _| {
                    (fit.value(Key::Ship, ATTR_MAX_LOCKED_TARGETS), fit.value(Key::Char, ATTR_MAX_LOCKED_TARGETS))
                })).await.unwrap().expect("limits");
                assert!(ship_limit <= 1.0 && pilot_limit >= 2.0, "ship={ship_limit} pilot={pilot_limit}");
                let cx = combat::ctx(&space.client).expect("scene");
                assert!(combat::targeting_charges::add_target_ctx(&cx, targets[0].ship, false).await.is_ok());
                until(&space.client.world, scene, 5_000, || combat::targeting_charges::targets_of(character).contains(&targets[0].ship)).await;
                match combat::targeting_charges::add_target_ctx(&cx, targets[1].ship, false).await {
                    Err(CallError::User { msg, dict }) => assert!(msg == messages::MSG_CUSTOM_NOTIFY && dict.iter().any(|(_, v)| v == &Value::unicode(messages::target_limit((ship_limit as usize).max(1)))), "{msg} {dict:?}"),
                    other => panic!("neutral second lock: {other:?}"),
                }
                let before = quantity(&db, stack).await;
                let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
                space.client.world.observe(scene, character, ship, destiny).await.unwrap();
                activate(&mut space, &effect_name(&sde, effect), mode).await;
                until(&space.client.world, scene, 5_000, || matches!(record(mode), Record::Running)).await;
                let arc = match combat::state().active_cycles.get(&mode) {
                    Some(ActiveCycle::Running { generation, effect: current, .. }) => { assert_eq!(*current, i64::from(effect)); generation.clone() },
                    _ => panic!("running"),
                };
                assert_eq!(space.client.world.with(scene, move |s| s.resident_fit(ship, |fit, _| (fit.source_type(Key::Module(mode)), fit.module_state(mode)))).await.unwrap(), Some((Some(type_id), Some((true, true)))));
                assert_eq!(quantity(&db, stack).await, before - effective_units(&space, mode).await);
                assert_eq!(fx_count(&mut events, mode, 1), 1);
                assert_eq!(godma(&drain(&mut space.rx), mode, i64::from(effect), 1), 1);
                let running_limit = ship_value(&space, ATTR_MAX_LOCKED_TARGETS).await;
                let raises = sde.dogma.effect(effect).unwrap().modifiers().iter().any(|row| row.modified == ATTR_MAX_LOCKED_TARGETS);
                if effect == EFFECT_TRIAGE {
                    assert!(raises && running_limit > ship_limit && running_limit >= 2.0, "{ship_limit} -> {running_limit}");
                    assert!(combat::targeting_charges::add_target_ctx(&cx, targets[1].ship, false).await.is_ok());
                    until(&space.client.world, scene, 5_000, || combat::targeting_charges::targets_of(character).contains(&targets[1].ship)).await;
                } else {
                    assert!(!raises && running_limit == ship_limit, "{ship_limit} -> {running_limit}");
                    assert!(matches!(combat::targeting_charges::add_target_ctx(&cx, targets[1].ship, false).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_CUSTOM_NOTIFY));
                }
                let mut held = combat::targeting_charges::targets_of(character);
                held.sort_unstable();
                let mut want = if effect == EFFECT_TRIAGE { vec![targets[0].ship, targets[1].ship] } else { vec![targets[0].ship] };
                want.sort_unstable();
                assert_eq!(held, want);
                assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation, .. }) if Arc::ptr_eq(generation, &arc)));
            }).await;
            for target in targets { cleanup_ship(&db, Some(&*space.client.world), target).await.unwrap(); }
            if let Err(panic) = nested { std::panic::resume_unwind(panic); }
        }).await;
        teardown(&db, space).await;
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }
    let fuel = ShipItemSpec { type_id: fuel_type(&sde, stellian), flag_id: FLAG_CARGO, quantity: raw_units(&sde, stellian), singleton: false, online: false, attributes: &[] };
    let mut space = in_space(&db, &sde, stellian, EFFECT_STELLIAN_TRIAGE, &[(duration(EFFECT_STELLIAN_TRIAGE), 8_000.0)], &[fuel], Some(high)).await;
    let result = crate::tests::catch_scenario(async {
        let (scene, mode, ship, stack) = (space.system, space.module, space.fixture.ship, space.fixture.items[1]);
        assert_eq!(scene, high);
        let before = quantity(&db, stack).await;
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        space.client.world.observe(scene, space.fixture.character, ship, destiny).await.unwrap();
        let refused = effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(mode), Value::str(&effect_name(&sde, EFFECT_STELLIAN_TRIAGE)), Value::None, Value::Int(0)]).await;
        assert!(matches!(refused, Err(CallError::User { msg, .. }) if msg == messages::MSG_CANT_IN_HIGH_SEC_SPACE));
        assert!(space.client.world.after(scene, 100).await);
        assert_eq!(quantity(&db, stack).await, before);
        assert_eq!(record(mode), Record::None);
        assert_eq!(space.client.world.with(scene, move |s| s.resident_fit(ship, |fit, _| fit.module_state(mode))).await.unwrap(), Some(Some((true, false))));
        assert_eq!(godma(&drain(&mut space.rx), mode, i64::from(EFFECT_STELLIAN_TRIAGE), 1), 0);
        assert_eq!(fx_count(&mut events, mode, 1), 0);
    }).await;
    teardown(&db, space).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn ordinary_triage_one_reaches_running_and_restores_after_paid_stop() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let ordinary_one = published_links(&sde, EFFECT_TRIAGE).into_iter().find(|type_id| {
        crate::services::skills::requirements::requirements(&sde, *type_id).any(|(_, level)| level == 1)
    }).expect("published ordinary triage with a level-1 module requirement");
    assert!(published_links(&sde, EFFECT_TRIAGE).iter().any(|type_id| {
        crate::services::skills::requirements::requirements(&sde, *type_id).any(|(_, level)| level == 5)
    }), "the other published ordinary module requires level 5");
    let fuel = ShipItemSpec { type_id: fuel_type(&sde, ordinary_one), flag_id: FLAG_CARGO, quantity: raw_units(&sde, ordinary_one) * 2, singleton: false, online: false, attributes: &[] };
    let mut space = in_space(&db, &sde, ordinary_one, EFFECT_TRIAGE, &[], &[fuel], None).await;
    let result = crate::tests::catch_scenario(async {
        let (system, mode, stack) = (space.system, space.module, space.fixture.items[1]);
        let attribute = sde.dogma.effect(EFFECT_TRIAGE).unwrap().modifiers().iter().find(|row| row.domain == "shipID").unwrap().modified;
        let base = ship_value(&space, attribute).await;
        let before = quantity(&db, stack).await;
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        space.client.world.observe(system, space.fixture.character, space.fixture.ship, destiny).await.unwrap();
        activate(&mut space, &effect_name(&sde, EFFECT_TRIAGE), mode).await;
        until(&space.client.world, system, 5_000, || matches!(record(mode), Record::Running)).await;
        let generation = match combat::state().active_cycles.get(&mode) {
            Some(ActiveCycle::Running { generation, .. }) => generation.clone(),
            _ => panic!("running"),
        };
        assert!(space.client.world.with(system, move |s| s.resident_fit(space.fixture.ship, |fit, _| fit.module_state(mode))).await.unwrap().flatten() == Some((true, true)), "the fitted module is online and active");
        assert!(quantity(&db, stack).await < before, "acceptance debits fuel");
        assert_eq!(fx_count(&mut events, mode, 1), 1, "one current start");
        let running = ship_value(&space, attribute).await;
        assert_ne!(running, base, "running mode changes the resident fit");
        effects_fixture::dogma_call(&mut space.client, "Deactivate", &[Value::Int(mode), Value::str(&effect_name(&sde, EFFECT_TRIAGE))]).await.ok().unwrap();
        assert_eq!(ship_value(&space, attribute).await, running, "the paid cycle keeps the bonus");
        assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation: current, .. }) if Arc::ptr_eq(current, &generation)), "the paid stop keeps the accepted generation");
        until(&space.client.world, system, 8_000, || record(mode) == Record::None).await;
        assert_eq!(ship_value(&space, attribute).await, base, "retirement restores the fit");
        assert_eq!(fx_count(&mut events, mode, 0), 1, "one current stop");
    }).await;
    teardown(&db, space).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
async fn damp_inputs(space: &InSpace, sender: i64, module: i64) -> std::sync::Arc<crate::services::effects::parameters::ProjectionInputs> {
    let effect = crate::services::effects::registry::resolve(&space.client.sde, infinity_core::consts::dogma::EFFECT_REMOTE_SENSOR_DAMP_FALLOFF).expect("damp");
    let captured = space.client.world.with(space.system, move |system| system.resident_fit(sender, |fit, _| crate::services::effects::parameters::capture_source(fit, Key::Module(module), &effect, 0.0))).await.unwrap().expect("resident damp").expect("projected capture");
    let crate::services::effects::parameters::SourceInputs::Projected(inputs) = captured else { panic!("damp capture") };
    inputs
}

mod fuel;
