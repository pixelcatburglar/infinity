//! Permanent coverage for the duration-bounded activations in `super`: pure
//! state transitions here, in-space consumer and boundary scenarios in the
//! child files, sharing the fixture and helpers below.
use super::*;
use crate::client::Client;
use crate::dao::items::dogma as item_dogma;
use crate::sde::Sde;
use crate::services::effects::tests::fixture as effects_fixture;
pub(super) use crate::services::effects::tests::fixture::{fitted_attr, module_online, ship_info_item};
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipFixture, ShipItemSpec, ShipLocation};
use crate::services::CallResult;
use infinity_core::consts::{self, CATEGORY_MODULE, CATEGORY_SHIP};
use infinity_core::consts::dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_CPU_OUTPUT, ATTR_DISALLOW_REPEATING_ACTIVATION,
    ATTR_POWER_OUTPUT, ATTR_RECHARGE_RATE};
/// Bounded fitted values keep every wait short in real time.
const DURATION_MS: f64 = 1_500.0;
const DELAY_MS: f64 = 2_000.0;

#[test]
fn admission_refuses_a_live_cooldown_and_expires_due_records() {
    let mut st = combat::State::default();
    let (module, type_id) = (42_001_i64, 9_i32);
    let now = now_ms();
    st.active_cycles.insert(module, ActiveCycle::Cooling { stopped: now - 500, deadline: now + 60_000 });
    st.cycle_deadlines.insert((now + 60_000, module));
    let Err(CallError::User { msg, dict }) = admission(&mut st, module, type_id) else { panic!("live cooling must refuse") };
    assert_eq!(msg, messages::MSG_MODULE_REACTIVATION_DELAYED);
    let lookup = |key: &str| dict.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let delay = lookup("delay").and_then(|v| v.as_float()).expect("delay entry");
    assert!(delay > 0.0 && delay <= 60.0, "remaining seconds: {delay}");
    assert_eq!(lookup("itemID").and_then(|v| v.as_int()), Some(module));
    assert!(lookup("timeSinceLastStop").and_then(|v| v.as_int()).is_some_and(|v| v >= 0));
    assert!(matches!(lookup("item"), Some(Value::Tuple(t)) if t.last().and_then(Value::as_int) == Some(i64::from(type_id))));
    st.active_cycles.insert(module, ActiveCycle::Cooling { stopped: now - 120_000, deadline: now - 1 });
    st.cycle_deadlines.clear(); st.cycle_deadlines.insert((now - 1, module));
    assert!(admission(&mut st, module, type_id).is_ok(), "expired deadline admits");
    assert!(!st.active_cycles.contains_key(&module) && st.cycle_deadlines.is_empty(), "record and index dropped");
}

#[test]
fn stopped_fences_on_the_record_generation() {
    let (module, ship, now) = (43_001_i64, 8_i64, now_ms());
    let (a, b) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let running = |generation: &Arc<AtomicBool>, delay_ms: i64| ActiveCycle::Running { generation: generation.clone(), owner: 1, ship, effect: i64::from(EFFECT_ASSAULT_DAMAGE_CONTROL), start: now - 1_000, duration_ms: DURATION_MS, delay_ms, repeat: 0 };
    let mut st = combat::State::default();
    st.active_cycles.insert(module, running(&a, 2_000));
    stopped(now, &mut st, module, &b);
    assert!(matches!(st.active_cycles.get(&module), Some(ActiveCycle::Running { generation, .. }) if Arc::ptr_eq(generation, &a)),
        "a stale generation leaves the record");
    assert!(st.cycle_deadlines.is_empty());
    stopped(now, &mut st, module, &a);
    assert!(matches!(st.active_cycles.get(&module), Some(ActiveCycle::Cooling { stopped: s, deadline: d }) if *s == now && *d == now + 2_000));
    assert_eq!(st.cycle_deadlines.len(), 1);
    assert!(st.cycle_deadlines.contains(&(now + 2_000, module)));
    st.active_cycles.insert(module, running(&a, 0));
    stopped(now, &mut st, module, &a);
    assert!(!st.active_cycles.contains_key(&module), "zero delay leaves no record");
    st.cycle_deadlines.clear(); let pending = Arc::new(AtomicBool::new(false));
    st.active_cycles.insert(module, ActiveCycle::Pending { generation: pending.clone(), ship });
    stopped(now, &mut st, module, &pending);
    assert!(!st.active_cycles.contains_key(&module) && st.cycle_deadlines.is_empty(), "pending cancels without cooldown");
}

#[test]
fn register_replaces_a_cooling_record_and_drops_its_index() {
    let mut st = combat::State::default();
    let (module, ship) = (44_001_i64, 9_i64);
    st.active_cycles.insert(module, ActiveCycle::Cooling { stopped: 1, deadline: 2_000 });
    st.cycle_deadlines.insert((2_000, module));
    let generation = Arc::new(AtomicBool::new(false));
    register(&mut st, module, ship, &generation);
    assert!(st.cycle_deadlines.is_empty());
    assert!(matches!(st.active_cycles.get(&module), Some(ActiveCycle::Pending { generation: g, ship: s }) if Arc::ptr_eq(g, &generation) && *s == ship));
}

/// The module's duration-bounded record in shared combat state.
#[derive(Debug, PartialEq)]
pub(super) enum Record { None, Pending, Running, Cooling { deadline: i64 } }

pub(super) fn record(module: i64) -> Record {
    match combat::state().active_cycles.get(&module) {
        None => Record::None,
        Some(ActiveCycle::Pending { .. }) => Record::Pending,
        Some(ActiveCycle::Running { .. }) => Record::Running,
        Some(ActiveCycle::Cooling { deadline, .. }) => Record::Cooling { deadline: *deadline },
    }
}
/// The ship attributes one consumer must lower through `ShipGetInfo`: the
/// assault damage control and hull energizer lower their resonance operands,
/// the suppressor lowers the ship's signature radius.
pub(super) fn watched(sde: &Sde, effect: i32, module: i64, ship: i64) -> Vec<(i64, i32)> {
    sde.dogma.effect(effect).expect("effect").modifiers().iter()
        .filter(|m| m.func == "ItemModifier")
        .map(|m| {
            let item = match m.domain.as_str() {
                "itemID" => module,
                "shipID" => ship,
                domain => panic!("unsupported consumer modifier domain {domain}"),
            };
            (item, m.modified)
        })
        .collect()
}

/// The first published module type carrying the consumer effect that the
/// activation path would admit.
pub(super) fn module_type(sde: &Sde, effect: i32) -> i32 {
    let resolved = crate::services::effects::registry::resolve(sde, effect).expect("consumer effect resolves");
    assert!(resolved.manually_activatable(), "consumer effect {effect} must be manually activatable");
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == effect)
        && crate::services::effects::registry::activation_in_scope(i64::from(t.category_id.unwrap_or(0)), i64::from(t.group_id.unwrap_or(0)), resolved.kind))
        .map(|t| t.type_id).expect("published module carrying the consumer effect")
}

/// Pending notifications collected so far.
pub(super) fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) -> Vec<(String, Vec<Value>)> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// `OnGodmaShipEffect` rows for (module, effect) with the given start flag.
pub(super) fn godma(ev: &[(String, Vec<Value>)], module: i64, effect: i64, start: i64) -> usize {
    ev.iter().filter(|(n, a)| n == "OnGodmaShipEffect" && a.first() == Some(&Value::Int(module))
        && a.get(1).and_then(Value::as_int) == Some(effect)
        && a.get(3).and_then(Value::as_int) == Some(start)).count()
}

/// Bounded wait: polls the condition once per 100 ms actor advance up to `ms`.
pub(super) async fn until(world: &crate::space::World, system: i64, ms: u64, mut ready: impl FnMut() -> bool) {
    for _ in 0..ms / 100 {
        if ready() { return; }
        assert!(world.after(system, 100).await, "system actor stopped");
    }
    assert!(ready(), "condition not met within {ms} ms");
}

/// The SDE name one activation passes on the wire.
pub(super) fn effect_name(sde: &Sde, effect: i32) -> String {
    sde.dogma.effect(effect).and_then(|e| e.name.clone()).expect("effect name")
}

/// The effect IDs the module's `activeEffects` lists through `ShipGetInfo`.
pub(super) async fn active_effects(client: &mut Client, item: i64) -> Vec<i64> {
    let fields = ship_info_item(client, item).await;
    let Some(Value::Dict(active)) = fields.get("activeEffects") else { panic!("activeEffects") };
    active.iter().filter_map(|(k, _)| k.as_int()).collect()
}

/// The bound ship-inventory `Add` to cargo, which is how the fitting window
/// removes a module.
pub(super) async fn unfit(client: &mut Client, module: i64, ship: i64) -> CallResult {
    let bind = Value::tuple([Value::Int(ship)]);
    let kwargs = Value::dict([("flag", Value::Int(i64::from(consts::FLAG_CARGO)))]);
    let args = [Value::Int(module), Value::Int(ship)];
    crate::services::dispatch(client, &crate::services::Call {
        service: crate::services::inventory::INVENTORY, method: "Add",
        args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) }).await
}

/// One in-space piloted hull with the consumer module online in its rack slot;
/// the fitted duration and reactivation delay are shrunk to bounded values.
pub(super) struct InSpace {
    pub fixture: ShipFixture,
    pub module: i64,
    pub client: Client,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>,
    pub system: i64,
}

/// A role-0 NPC-corporation pilot in space, following the active-attribute
/// pattern: create docked, move to space, skills from requirements, admit.
pub(super) async fn in_space(db: &crate::db::Db, sde: &Arc<Sde>, module_type: i32, effect: i32) -> InSpace {
    let system = sde.map.stations().find(|s| s.solar_system_id.is_some()
        && sde.map.celestial(s.station_id).is_some_and(|c| c.x.is_some() && c.y.is_some() && c.z.is_some()))
        .and_then(|s| s.solar_system_id).expect("station with celestial");
    let data = sde.types.get(module_type).expect("fixture module type");
    // The canFitShipGroup*/canFitShipType* restriction values on the module
    // name the hulls it may be fitted on.
    let can_fit = |name: &dyn Fn(i32) -> String| (1..).map_while(|i| sde.dogma.attributes_by_name(&name(i)).next())
        .filter_map(|a| data.attribute(a.attribute_id)).map(|v| v as i32).filter(|v| *v > 0).collect::<Vec<i32>>();
    let (groups, types) = (can_fit(&|i| format!("canFitShipGroup{i:02}")), can_fit(&|i| format!("canFitShipType{i}")));
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && (groups.is_empty() && types.is_empty() || groups.contains(&t.group_id.unwrap_or(0)) || types.contains(&t.type_id)))
        .map(|t| t.type_id).expect("published hull the fixture module fits");
    let flag = crate::services::effects::fitting::rack(&sde.types, module_type).map(|(_, _, first)| first).expect("module rack");
    let duration = sde.dogma.effect(effect).and_then(|e| e.duration_attribute_id).expect("duration attribute");
    let f = create_ship(db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: module_type, flag_id: flag,
        quantity: 1, singleton: true, online: true,
        attributes: &[(duration, DURATION_MS), (ATTR_REACTIVATION_DELAY, DELAY_MS), (ATTR_DISALLOW_REPEATING_ACTIVATION, 1.0)] }]).await.unwrap();
    let x = sde.map.celestials(system).map(|c| c.x.expect("celestial x") + f64::from(sde.celestial_radius(c))).reduce(f64::max).expect("system celestials") + 5_000.0;
    db.execute("UPDATE game.items SET location_id=$2,x=$3,y=0,z=0 WHERE item_id=$1", &[&f.ship, &system, &x]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2, solar_system_id=$3, station_id=NULL WHERE character_id=$1", &[&f.character, &f.ship, &system]).await.unwrap();
    for (a, v) in [(ATTR_CAPACITOR_CAPACITY, 1e9), (ATTR_RECHARGE_RATE, 1e6), (ATTR_CPU_OUTPUT, 1e9), (ATTR_POWER_OUTPUT, 1e9)] {
        item_dogma::set_override(db, f.ship, a, v).await.unwrap();
    }
    let (mut client, _peer) = crate::services::tests::client(db.clone(), f.character).await;
    client.session.apply(vec![("userid", Value::Int(f.account)), ("role", Value::Int(0)), ("corpid", Value::Int(1)),
        ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    client.login.user_id = f.account;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx.clone();
    client.world.register_pilot(f.character, tx);
    client.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&client.session));
    // Grant the module and hull skill requirements through the GM path.
    let required = [module_type, hull].into_iter().flat_map(|t| crate::services::skills::requirements::requirements(sde, t))
        .fold(std::collections::BTreeMap::new(), |mut m, (s, l)| { m.entry(s).and_modify(|v: &mut i32| *v = (*v).max(l)).or_insert(l); m });
    crate::services::skills::gm::apply(&mut client, f.character, crate::services::skills::gm::Change::Set(required.into_iter().collect())).await.unwrap();
    effects_fixture::admit(db, &client.world, system, effects_fixture::ball(f.ship, hull, f.character, Some(f.character), x)).await;
    InSpace { module: f.module(), fixture: f, client, rx, system }
}

/// Stop the fixture's activations, drop its combat records and remove its rows.
pub(super) async fn teardown(db: &crate::db::Db, space: InSpace) {
    let InSpace { fixture, module, client, system, .. } = space;
    crate::services::combat::reset(&client);
    let _ = client.world.after(system, 200).await;
    {
        let mut st = combat::state();
        st.active_cycles.remove(&module);
        st.cycle_deadlines.retain(|&(_, other)| other != module);
        if let Some(a) = st.active.remove(&module) { a.stop.store(true, Ordering::Relaxed); }
        st.activation_generation.remove(&module);
    }
    cleanup_ship(db, Some(&*client.world), fixture).await.unwrap();
}

/// Fixture in space, the body under `catch_scenario`, torn down before a panic unwinds.
pub(super) async fn scenario(effect: i32, body: impl std::ops::AsyncFnOnce(&mut InSpace, &Arc<Sde>, &crate::db::Db)) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let mut space = in_space(&db, &sde, module_type(&sde, effect), effect).await;
    let result = crate::tests::catch_scenario(body(&mut space, &sde, &db)).await;
    teardown(&db, space).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

/// The `[module, name, None, 100]` Activate argument list.
pub(super) fn activate_args(space: &InSpace, name: &str) -> Vec<Value> {
    vec![Value::Int(space.module), Value::str(name), Value::None, Value::Int(100)]
}

mod boundaries;
mod consumers;
