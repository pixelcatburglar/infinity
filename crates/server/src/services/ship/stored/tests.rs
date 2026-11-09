//! Real bound stored-ship transitions against a dedicated initialized database.
mod recovery;
mod settlement;

use infinity_core::consts::{CATEGORY_SHIP, FLAG_SHIP_HANGAR, GROUP_CAPSULE, GROUP_SOLAR_SYSTEM, TYPE_CAPSULE};
use infinity_core::consts::dogma::{ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY};
use crate::dao::items::{creation, loading::{load_items, ItemQuery}, ship_configuration::{self, Patch}, space_ships::FreeShip};
use crate::services::{Call, CallResult, drones::tests::fixture::Fixture};
use marshal::Value;

struct BayFixture {
    f: Fixture,
    host: i64,
    host_owner: i64,
    host_pilot: i64,
    kind: i32,
    notices: Vec<tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>>,
}

impl BayFixture {
    async fn new() -> Self {
        let mut f = Fixture::new().await;
        let system_data = f.sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).expect("SDE system hierarchy");
        let (constellation, region) = (system_data.constellation_id.unwrap(), system_data.region_id.unwrap());
        crate::dao::characters::ship::location(&f.db, f.owner, f.system, constellation, region).await.unwrap();
        let host_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
            && t.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
            && super::super::balls::radius(&f.sde, t.type_id).is_ok()).expect("SDE maintenance bay host");
        let capacity = host_type.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).unwrap();
        let host_kind = host_type.type_id;
        let kind = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
            && t.type_id != host_kind && t.group_id != Some(GROUP_CAPSULE) && t.volume.is_some_and(|v| v > 0.0 && v <= capacity)
            && crate::services::inventory::holds::accepts(&f.sde, FLAG_SHIP_HANGAR, t.type_id)
            && super::super::balls::radius(&f.sde, t.type_id).is_ok() && f.sde.is_tradable(t.type_id)
            && ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).any(|(&skill, level)| t.attribute(skill).unwrap_or(0.0) > 0.0 && t.attribute(level).unwrap_or(0.0) > 0.0))
            .expect("tradable SDE ship fitting maintenance bay").type_id;
        let (host_owner, host_pilot) = f.add_pilot().await;
        crate::dao::characters::ship::location(&f.db, host_owner, f.system, constellation, region).await.unwrap();
        let pos = [0.0; 3];
        let old = creation::positioned(&f.db, kind, f.owner, f.system, pos).await.unwrap();
        assert!(crate::dao::items::dogma::set_override(&f.db, old, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await.unwrap());
        crate::dao::characters::selection::set_ship(&f.db, f.owner, old).await.unwrap();
        f.world.remove_ball(f.system, f.ship).await;
        f.ship = old;
        f.client.session.apply(vec![("shipid", Value::Int(old)), ("userid", Value::Int(f.account)), ("solarsystemid2", Value::Int(f.system))]);
        let online = infinity_core::session::OnlineSession::from_session(&f.client.session).expect("fixture online session");
        f.world.set_online_session(f.owner, Some(online));
        let row = FreeShip { item_id: old, type_id: kind, owner_id: f.owner, corporation_id: 1, alliance_id: -1, pos };
        let pilot = super::super::balls::piloted(&f.db, f.sde.clone(), &row, f.owner, 0.0).await.unwrap();
        f.world.with(f.system, move |s| pilot.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        f.world.observe(f.system, f.client.login.client_id, old, f.client.destiny_tx.clone()).await.unwrap();
        let host = creation::positioned(&f.db, host_kind, host_owner, f.system, pos).await.unwrap();
        let row = FreeShip { item_id: host, type_id: host_kind, owner_id: host_owner, corporation_id: 1, alliance_id: -1, pos };
        let ball = super::super::balls::unpiloted(&f.db, f.sde.clone(), &row).await.unwrap();
        f.world.with(f.system, move |s| ball.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
        f.db.transaction(|tx, _: &mut ()| Box::pin(async move {
            assert!(crate::dao::items::ship::lock_reload_ship(tx, host).await?);
            assert!(ship_configuration::patch(tx, host, host_owner, &Patch { maintenance_corp: Some(true), ..Patch::default() }).await?);
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async move { Ok(()) }).await.unwrap();
        let ty = f.sde.types.get(kind).unwrap();
        let levels: Vec<_> = ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).filter_map(|(&skill, level)| {
            let id = ty.attribute(skill).unwrap_or(0.0) as i32;
            let level = ty.attribute(level).unwrap_or(0.0) as i32;
            (id > 0 && level > 0).then_some((id, level))
        }).collect();
        let sde = f.sde.clone();
        let owner = f.owner;
        let world = f.world.clone();
        let market = crate::services::market::lock().await;
        let fences = crate::services::effects::online::capture_online_fences(&world, &[owner]).await;
        f.db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone(); let levels = levels.clone(); let fences = fences.clone();
            Box::pin(async move { crate::services::skills::gm::set_levels(tx, outbox, &sde, &fences, owner, &levels, crate::client::filetime_now()).await })
        }, |(), outbox| async move { crate::services::effects::online::dispatch_changed_world((), outbox, &world) }).await.unwrap();
        drop(market);
        let fixture = Self { f, host, host_owner, host_pilot, kind, notices: Vec::new() };
        assert!(active_armor_hull(&fixture, fixture.f.ship).await.is_some(), "live actor condition");
        fixture
    }

    async fn stored(&self, owner: i64) -> i64 {
        creation::rookie_ship(&self.f.db, self.kind, owner, self.host, FLAG_SHIP_HANGAR).await.unwrap()
    }

    async fn capture_refusals(&mut self) {
        let (viewer, _) = self.f.add_pilot().await;
        for owner in [self.f.owner, self.host_owner, viewer] {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            if owner == self.f.owner { self.f.client.notify_tx = tx.clone(); }
            self.f.world.register_pilot(owner, tx);
            if owner == self.f.owner {
                let online = infinity_core::session::OnlineSession::from_session(&self.f.client.session).expect("fixture online session");
                self.f.world.set_online_session(owner, Some(online));
            }
            self.notices.push(rx);
        }
    }

    async fn refuse(&mut self, method: &str, args: &[Value], bind: Option<&Value>) {
        assert_eq!(self.notices.len(), 3);
        assert!(self.notices.iter().all(|rx| !rx.is_closed()));
        let counts: Vec<_> = self.notices.iter().map(|rx| rx.len()).collect();
        let params = Value::tuple([Value::Int(self.f.system), Value::Int(GROUP_SOLAR_SYSTEM)]);
        let result = self.call_bound(method, args, bind.unwrap_or(&params)).await;
        assert!(result.is_err(), "{method} unexpectedly succeeded: {result:?}");
        assert!(self.notices.iter().all(|rx| !rx.is_closed()));
        assert_eq!(counts, self.notices.iter().map(|rx| rx.len()).collect::<Vec<_>>(), "refusal emitted native notices");
    }

    async fn call_bound(&mut self, method: &str, args: &[Value], params: &Value) -> CallResult {
        let before = self.snapshot().await;
        let kwargs = Value::None;
        let result = crate::services::dispatch(&mut self.f.client, &Call { service: "ship", method, args, kwargs: &kwargs, bound: true, bind_params: Some(params) }).await;
        if result.is_err() { assert_eq!(before, self.snapshot().await, "refusal mutated fixture state"); }
        result
    }

    async fn call(&mut self, method: &str, args: &[Value]) -> CallResult {
        let params = Value::tuple([Value::Int(self.f.system), Value::Int(GROUP_SOLAR_SYSTEM)]);
        self.call_bound(method, args, &params).await
    }

    async fn snapshot(&self) -> String {
        use crate::dao::items::{inventory, dogma, pending_reloads};
        let mut ids = inventory::descendants(&self.f.db, &[self.f.ship, self.host]).await.unwrap();
        let owned = load_items(&self.f.db, ItemQuery::Owner(self.f.owner)).await.unwrap();
        ids.extend(owned.iter().map(|item| item.item_id));
        ids.sort_unstable();
        ids.dedup();
        let items = load_items(&self.f.db, ItemQuery::Ids(&ids)).await.unwrap();
        let rows: Vec<_> = items.iter().map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, &i.custom_info)).collect();
        let attrs = dogma::overrides_for(&self.f.db, &ids).await.unwrap();
        let attrs: Vec<_> = attrs.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect();
        let pending = pending_reloads::intersecting(&self.f.db, &ids).await.unwrap();
        let pending: Vec<_> = pending.iter().map(|p| (p.reload_id, p.owner_id, p.ship_id, p.module_id, p.flag_id, p.type_id, p.quantity, p.source_flag, p.source_location_id)).collect();
        let selection = crate::dao::characters::selection::ship_id(&self.f.db, self.f.owner).await.unwrap();
        let host_selection = crate::dao::characters::selection::ship_id(&self.f.db, self.host_owner).await.unwrap();
        let condition = active_armor_hull(self, self.f.ship).await;
        let session: Vec<_> = self.f.client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let owner = self.f.owner;
        let world = self.f.world.with(self.f.system, move |s| {
            let mut balls: Vec<_> = s.balls().map(|b| (b.id, b.type_id, b.owner_id, b.char_id, b.interactive)).collect();
            balls.sort_unstable();
            (s.pilot_ball(owner).map(|b| b.id), balls.iter().map(|b| (b.0, s.fleet_broadcast_visible(owner, b.0))).collect::<Vec<_>>(), balls)
        }).await;
        format!("{rows:?} {attrs:?} {pending:?} {selection:?} {host_selection:?} {condition:?} {session:?} {world:?} {}", self.f.client.outbox.len())
    }

    async fn cleanup(self) { self.f.cleanup().await; }
}

async fn active_armor_hull(f: &BayFixture, ship: i64) -> Option<(f64, f64)> {
    f.f.world.with(f.f.system, move |s| s.ship_condition(ship)).await.flatten()
        .map(|(_, armor, hull)| (armor, hull))
}

async fn replace_ball_preserving_condition(f: &BayFixture, ball: crate::space::Ball) {
    let id = ball.id;
    let fit = if let Some(character) = ball.char_id {
        crate::services::fit::Fit::load(&f.f.db, f.f.sde.clone(), character, id).await.unwrap()
    } else {
        crate::services::fit::Fit::load_unpiloted(&f.f.db, f.f.sde.clone(), id).await.unwrap()
    };
    f.f.world.with(f.f.system, move |s| {
        let condition = s.combat_condition_snapshot(id).expect("live condition before replacement");
        if ball.char_id.is_some() {
            s.add_ship(ball);
        } else {
            s.take(id);
            s.add_persistent_ship(ball);
        }
        let generation = s.persistent_generation(id).expect("replacement membership");
        assert!(s.transfer_combat_state(id, generation, condition, fit, crate::services::fit::Key::Ship));
        assert!(s.ship_condition(id).is_some());
    }).await.unwrap();
}

async fn row(f: &BayFixture, id: i64) -> crate::dao::items::loading::GameItem {
    load_items(&f.f.db, ItemQuery::Id(id)).await.unwrap().pop().expect("item row")
}

fn state(value: Value) {
    let Value::Tuple(fields) = value else { panic!("expected canonical ship state") };
    assert_eq!(fields.len(), 4);
}

async fn selected(f: &BayFixture, ship: i64) {
    assert_eq!(f.f.client.session.int("shipid"), Some(ship));
    assert_eq!(crate::dao::characters::selection::ship_id(&f.f.db, f.f.owner).await.unwrap(), Some(ship));
    let owner = f.f.owner;
    f.f.world.with(f.f.system, move |s| {
        assert_eq!(s.pilot_ball(owner).map(|b| b.id), Some(ship));
        assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
        assert!(s.fleet_broadcast_visible(owner, ship), "observer ego must track selected ship");
    }).await.unwrap();
}

async fn cargo(f: &BayFixture, ship: i64, owner: i64) -> i64 {
    let kind = f.f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
        && f.f.sde.is_tradable(t.type_id)).expect("tradable SDE cargo").type_id;
    crate::dao::items::transfers::insert(&f.f.db, kind, owner, ship, infinity_core::consts::FLAG_CARGO, 3).await.unwrap()
}

async fn assert_cargo(f: &BayFixture, id: i64, ship: i64, owner: i64) {
    let item = row(f, id).await;
    assert_eq!((item.owner_id, item.location_id, item.flag_id, item.quantity), (owner, ship, infinity_core::consts::FLAG_CARGO, 3));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn store_switches_to_capsule_and_removes_stored_world_hull() {
    let mut b = BayFixture::new().await;
    let old = b.f.ship;
    let host = b.host;
    let content = cargo(&b, old, b.f.owner).await;
    state(b.call("StoreVessel", &[Value::Int(host)]).await.unwrap());
    let capsule = b.f.client.session.int("shipid").unwrap();
    assert_ne!(capsule, old);
    let saved = row(&b, old).await;
    assert_eq!((saved.owner_id, saved.location_id, saved.flag_id, saved.singleton, saved.quantity), (b.f.owner, host, FLAG_SHIP_HANGAR, true, 1));
    assert_eq!(row(&b, capsule).await.type_id, TYPE_CAPSULE);
    assert!(b.f.world.ball(b.f.system, old).await.is_none());
    assert_cargo(&b, content, old, b.f.owner).await;
    assert!(crate::dao::items::dogma::overrides(&b.f.db, old).await.unwrap().iter().any(|a| a.attribute_id == infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE && a.value == 1.0));
    selected(&b, capsule).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn board_stored_takes_hull_and_retains_previous_unpiloted() {
    let mut b = BayFixture::new().await;
    let old = b.f.ship;
    let host = b.host;
    let target = b.stored(b.host_owner).await;
    let content = cargo(&b, target, b.host_owner).await;
    assert!(crate::dao::items::dogma::set_override(&b.f.db, target, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await.unwrap());
    state(b.call("BoardStoredShip", &[Value::Int(host), Value::Int(target)]).await.unwrap());
    let item = row(&b, target).await;
    assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.f.owner, b.f.system, 0));
    assert_cargo(&b, content, target, b.f.owner).await;
    assert!(crate::dao::items::dogma::overrides(&b.f.db, target).await.unwrap().iter().any(|a| a.attribute_id == infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE && a.value == 1.0));
    let old_ball = b.f.world.ball(b.f.system, old).await.unwrap();
    assert_eq!(old_ball.char_id, None);
    assert!(!old_ball.interactive);
    selected(&b, target).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn launch_stored_takes_hull_without_switching_ego() {
    let mut b = BayFixture::new().await;
    let old = b.f.ship;
    let host = b.host;
    let target = b.stored(b.host_owner).await;
    let content = cargo(&b, target, b.host_owner).await;
    assert!(crate::dao::items::dogma::set_override(&b.f.db, target, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await.unwrap());
    assert_eq!(b.call("LaunchFromContainer", &[Value::Int(host), Value::Int(target)]).await.unwrap(), Value::None);
    let item = row(&b, target).await;
    assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.f.owner, b.f.system, 0));
    assert_cargo(&b, content, target, b.f.owner).await;
    assert!(crate::dao::items::dogma::overrides(&b.f.db, target).await.unwrap().iter().any(|a| a.attribute_id == infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE && a.value == 1.0));
    let ball = b.f.world.ball(b.f.system, target).await.unwrap();
    assert_eq!((ball.owner_id, ball.char_id, ball.interactive), (b.f.owner, None, false));
    assert!(ball.pos.iter().all(|v| v.is_finite()));
    selected(&b, old).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn missing_wrong_bay_and_capsule_targets_are_atomic() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let old = b.f.ship;
    let target = b.stored(b.host_owner).await;
    let capsule = creation::rookie_ship(&b.f.db, TYPE_CAPSULE, b.host_owner, host, FLAG_SHIP_HANGAR).await.unwrap();
    let wrong_bay = creation::rookie_ship(&b.f.db, b.kind, b.host_owner, host, infinity_core::consts::FLAG_CARGO).await.unwrap();
    for method in ["BoardStoredShip", "LaunchFromContainer"] {
        for (container, item) in [(host, i64::MAX), (i64::MAX, target), (host, capsule), (host, wrong_bay), (host, old)] {
            let before = row(&b, target).await;
            let queued = b.f.client.outbox.len();
            b.refuse(method, &[Value::Int(container), Value::Int(item)], None).await;
            let after = row(&b, target).await;
            assert_eq!((before.owner_id, before.location_id, before.flag_id, before.quantity, before.singleton),
                       (after.owner_id, after.location_id, after.flag_id, after.quantity, after.singleton));
            assert_eq!(b.f.client.outbox.len(), queued);
            selected(&b, old).await;
            assert!(b.f.world.ball(b.f.system, target).await.is_none());
        }
    }
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn maintenance_grant_is_independent_and_revocation_is_atomic() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let owner = b.host_owner;
    let target = b.stored(owner).await;
    let old = b.f.ship;
    let patch = Patch { fleet_hangar_corp: Some(true), maintenance_corp: Some(false), ..Patch::default() };
    assert!(ship_configuration::patch(&b.f.db, host, owner, &patch).await.unwrap());
    for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
        let args = if method == "StoreVessel" { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
        let queued = b.f.client.outbox.len();
        b.refuse(method, &args, None).await;
        assert_eq!(b.f.client.outbox.len(), queued);
        let item = row(&b, target).await;
        assert_eq!((item.owner_id, item.location_id, item.flag_id), (owner, host, FLAG_SHIP_HANGAR));
        selected(&b, old).await;
        assert!(b.f.world.ball(b.f.system, target).await.is_none());
    }
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn stale_binding_hidden_host_and_outside_range_are_atomic() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let target = b.stored(b.host_owner).await;
    let old = b.f.ship;
    let args = [Value::Int(host), Value::Int(target)];
    for bind in [Value::tuple([Value::Int(b.f.system + 1), Value::Int(GROUP_SOLAR_SYSTEM)]), Value::None] {
        let queued = b.f.client.outbox.len();
        b.refuse("LaunchFromContainer", &args, Some(&bind)).await;
        assert_eq!(b.f.client.outbox.len(), queued);
        selected(&b, old).await;
    }
    let pilot = b.f.world.ball(b.f.system, old).await.unwrap();
    let original = b.f.world.ball(b.f.system, host).await.unwrap();
    let mut hidden = original.clone();
    hidden.cloak = 1;
    let mut far = original.clone();
    let distance = infinity_core::consts::MAX_CARGO_CONTAINER_TRANSFER_DISTANCE + f64::from(pilot.radius) + f64::from(far.radius) + 1.0;
    far.pos = [pilot.pos[0] + distance, pilot.pos[1], pilot.pos[2]];
    far.old_pos = far.pos;
    for ball in [hidden, far] {
        replace_ball_preserving_condition(&b, ball).await;
        let queued = b.f.client.outbox.len();
        for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
            let args = if method == "StoreVessel" { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
            b.refuse(method, &args, None).await;
            assert_eq!(b.f.client.outbox.len(), queued);
            let item = row(&b, target).await;
            assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.host_owner, host, FLAG_SHIP_HANGAR));
            assert!(b.f.world.ball(b.f.system, target).await.is_none());
            selected(&b, old).await;
        }
    }
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn self_store_and_stale_selected_ship_are_atomic() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let old = b.f.ship;
    let host = b.host;
    let target = b.stored(b.host_owner).await;
    let before = b.f.client.outbox.len();
    b.refuse("StoreVessel", &[Value::Int(old)], None).await;
    selected(&b, old).await;
    b.f.client.session.apply(vec![("shipid", Value::Int(target))]);
    for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
        let args = if method == "StoreVessel" { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
        b.refuse(method, &args, None).await;
        assert_eq!(b.f.client.outbox.len(), before);
        let item = row(&b, target).await;
        assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.host_owner, host, FLAG_SHIP_HANGAR));
        assert!(b.f.world.ball(b.f.system, target).await.is_none());
    }
    b.f.client.session.apply(vec![("shipid", Value::Int(old))]);
    selected(&b, old).await;
    b.cleanup().await;
}

async fn assert_grant_transition(b: &mut BayFixture, method: &str) {
    let host = b.host;
    let old = b.f.ship;
    let actor = b.f.owner;
    if method == "StoreVessel" {
        state(b.call(method, &[Value::Int(host)]).await.unwrap());
        let capsule = b.f.client.session.int("shipid").unwrap();
        assert_ne!(capsule, old);
        let stored = row(b, old).await;
        assert_eq!((stored.owner_id, stored.location_id, stored.flag_id), (actor, host, FLAG_SHIP_HANGAR));
        assert_eq!(row(b, capsule).await.type_id, TYPE_CAPSULE);
        assert!(b.f.world.ball(b.f.system, old).await.is_none());
        selected(b, capsule).await;
    } else {
        let target = b.stored(b.host_owner).await;
        let result = b.call(method, &[Value::Int(host), Value::Int(target)]).await.unwrap();
        let taken = row(b, target).await;
        assert_eq!((taken.owner_id, taken.location_id, taken.flag_id), (actor, b.f.system, 0));
        if method == "BoardStoredShip" {
            state(result);
            assert_eq!(b.f.world.ball(b.f.system, old).await.unwrap().char_id, None);
            selected(b, target).await;
        } else {
            assert_eq!(result, Value::None);
            let ball = b.f.world.ball(b.f.system, target).await.unwrap();
            assert_eq!((ball.owner_id, ball.char_id, ball.interactive), (actor, None, false));
            selected(b, old).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn fleet_maintenance_grant_works_without_corporation_grant() {
    for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
        let mut b = BayFixture::new().await;
        let host = b.host;
        let owner = b.host_owner;
        let pilot = b.host_pilot;
        crate::services::drones::tests::resources::fleet::form_fleet(&mut b.f, owner, pilot).await.unwrap();
        let patch = Patch { maintenance_corp: Some(false), maintenance_fleet: Some(true), ..Patch::default() };
        assert!(ship_configuration::patch(&b.f.db, host, owner, &patch).await.unwrap());
        assert_grant_transition(&mut b, method).await;
        b.cleanup().await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn host_owner_grant_does_not_require_any_switch() {
    for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
        let mut b = BayFixture::new().await;
        let host = b.host;
        let actor = b.f.owner;
        let item = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(host)).await.unwrap().pop().unwrap();
        assert!(crate::dao::items::inventory::move_item(&b.f.db, &item, b.f.system, 0, actor, true).await.unwrap());
        let mut ball = b.f.world.ball(b.f.system, host).await.unwrap();
        ball.owner_id = actor;
        replace_ball_preserving_condition(&b, ball).await;
        assert_grant_transition(&mut b, method).await;
        b.cleanup().await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn full_bay_packaged_nonship_and_selected_targets_are_atomic() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let old = b.f.ship;
    let target = b.stored(b.host_owner).await;
    let packaged = crate::dao::items::transfers::insert(&b.f.db, b.kind, b.host_owner, host, FLAG_SHIP_HANGAR, 2).await.unwrap();
    let packaged_one = crate::dao::items::transfers::insert(&b.f.db, b.kind, b.host_owner, host, FLAG_SHIP_HANGAR, 1).await.unwrap();
    let nonship_type = b.f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    let nonship = creation::rookie_ship(&b.f.db, nonship_type, b.host_owner, host, FLAG_SHIP_HANGAR).await.unwrap();
    let before = b.f.client.outbox.len();
    for method in ["BoardStoredShip", "LaunchFromContainer"] {
        for item in [packaged, packaged_one, nonship] {
            b.refuse(method, &[Value::Int(host), Value::Int(item)], None).await;
            assert_eq!(b.f.client.outbox.len(), before);
            selected(&b, old).await;
        }
    }
    crate::dao::characters::selection::set_ship(&b.f.db, b.host_owner, target).await.unwrap();
    for method in ["BoardStoredShip", "LaunchFromContainer"] {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            b.refuse(method, &[Value::Int(host), Value::Int(target)], None),
        ).await.expect("selected-target refusal timed out");
        assert_eq!(b.f.client.outbox.len(), before);
        selected(&b, old).await;
    }
    crate::dao::characters::selection::set_ship(&b.f.db, b.host_owner, b.host_pilot).await.unwrap();
    b.f.set(host, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY, 0.0).await;
    b.refuse("StoreVessel", &[Value::Int(host)], None).await;
    assert_eq!(b.f.client.outbox.len(), before);
    let item = row(&b, target).await;
    assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.host_owner, host, FLAG_SHIP_HANGAR));
    selected(&b, old).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn launch_accepts_exact_surface_range() {
    let mut b = BayFixture::new().await;
    let host = b.host;
    let old = b.f.ship;
    let target = b.stored(b.host_owner).await;
    let (done, ready) = tokio::sync::oneshot::channel();
    b.f.world.with(b.f.system, move |s| {
        let pilot_pos = s.sample_position(old).unwrap();
        let distance = infinity_core::consts::MAX_CARGO_CONTAINER_TRANSFER_DISTANCE
            + f64::from(s.ball(old).unwrap().radius) + f64::from(s.ball(host).unwrap().radius);
        // Coincident fixture hulls separate along X, leaving Y exact for the boundary.
        let host_pos = [pilot_pos[0], pilot_pos[1] + distance, pilot_pos[2]];
        s.command(old, move |ball, _, _| {
            ball.stop();
            ball.pos = pilot_pos;
            ball.vel = [0.0; 3];
            Vec::new()
        });
        s.command(host, move |ball, _, _| {
            ball.stop();
            ball.pos = host_pos;
            ball.vel = [0.0; 3];
            let _ = done.send(());
            Vec::new()
        });
    }).await.unwrap();
    ready.await.unwrap();
    let evidence = b.f.world.with(b.f.system, move |s| {
        let pilot = s.ball(old).unwrap();
        let host = s.ball(host).unwrap();
        let pilot_sampled = s.sample_position(old).unwrap();
        let host_sampled = s.sample_position(host.id).unwrap();
        let gap = crate::space::dist(pilot_sampled, host_sampled) - f64::from(pilot.radius) - f64::from(host.radius);
        (s.now_ms(), pilot.pos, host.pos, pilot_sampled, host_sampled, pilot.radius, host.radius, gap, infinity_core::consts::MAX_CARGO_CONTAINER_TRANSFER_DISTANCE)
    }).await.unwrap();
    assert!(evidence.7 == evidence.8, "sampled surface range is not exact: {evidence:?}");
    let result = b.call("LaunchFromContainer", &[Value::Int(host), Value::Int(target)]).await;
    assert!(matches!(&result, Ok(Value::None)), "launch result: {result:?}; (ms, pilot raw, host raw, pilot sampled, host sampled, pilot radius, host radius, gap, limit): {evidence:?}");
    selected(&b, old).await;
    assert_eq!(row(&b, target).await.owner_id, b.f.owner);
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn warped_or_cloaked_actor_cannot_use_stored_lifecycle() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let old = b.f.ship;
    let target = b.stored(b.host_owner).await;
    let original = b.f.world.ball(b.f.system, old).await.unwrap();
    let mut warped = original.clone();
    assert_eq!(warped.warp_to([10.0 * infinity_core::consts::AU, 0.0, 0.0], 0.0, 1), crate::space::WarpOutcome::Align);
    let mut cloaked = original.clone();
    cloaked.cloak = 1;
    for (scenario, ball) in [("warped", warped), ("cloaked", cloaked)] {
        replace_ball_preserving_condition(&b, ball).await;
        assert!(active_armor_hull(&b, old).await.is_some());
        selected(&b, old).await;
        let queued = b.f.client.outbox.len();
        for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
            let args = if method == "StoreVessel" { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
            let owner = b.f.owner;
            let pilot = b.f.world.with(b.f.system, move |s| {
                s.pilot_ball(owner).map(|p| (p.id, p.mode, p.cloak, p.char_id, p.owner_id))
            }).await.expect("system running");
            assert!(pilot.is_some_and(|(id, mode, cloak, char_id, owner_id)| {
                id == old && char_id == Some(owner) && owner_id == owner
                    && if scenario == "warped" { mode == crate::space::MODE_WARP } else { cloak != 0 }
            }), "{scenario} before {method}: indexed pilot (id, mode, cloak, char_id, owner_id) = {pilot:?}");
            b.refuse(method, &args, None).await;
            assert_eq!(b.f.client.outbox.len(), queued);
            assert_eq!(crate::dao::characters::selection::ship_id(&b.f.db, b.f.owner).await.unwrap(), Some(old));
            let item = row(&b, target).await;
            assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.host_owner, host, FLAG_SHIP_HANGAR));
            assert!(b.f.world.ball(b.f.system, target).await.is_none());
        }
    }
    replace_ball_preserving_condition(&b, original).await;
    selected(&b, old).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn stale_account_docked_and_wrong_system_are_atomic() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let old = b.f.ship;
    let target = b.stored(b.host_owner).await;
    let account = b.f.client.login.user_id;
    for scenario in 0..3 {
        match scenario {
            0 => b.f.client.login.user_id = i64::MAX,
            1 => { b.f.client.session.apply(vec![("stationid", Value::Int(i64::MAX))]); }
            _ => { b.f.client.session.apply(vec![("solarsystemid", Value::Int(b.f.system + 1))]); }
        }
        let queued = b.f.client.outbox.len();
        for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
            let args = if method == "StoreVessel" { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
            b.refuse(method, &args, None).await;
            assert_eq!(b.f.client.outbox.len(), queued);
            let item = row(&b, target).await;
            assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.host_owner, host, FLAG_SHIP_HANGAR));
            assert_eq!(crate::dao::characters::selection::ship_id(&b.f.db, b.f.owner).await.unwrap(), Some(old));
            assert!(b.f.world.ball(b.f.system, target).await.is_none());
        }
        b.f.client.login.user_id = account;
        b.f.client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(b.f.system))]);
        selected(&b, old).await;
    }
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn stale_host_database_location_or_world_type_refuses_atomically() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let old = b.f.ship;
    let target = b.stored(b.host_owner).await;
    let item = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(host)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(&b.f.db, &item, b.f.system + 1, 0, b.host_owner, true).await.unwrap());
    let queued = b.f.client.outbox.len();
    for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
        let args = if method == "StoreVessel" { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
        b.refuse(method, &args, None).await;
        assert_eq!(b.f.client.outbox.len(), queued);
        selected(&b, old).await;
    }
    let item = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(host)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(&b.f.db, &item, b.f.system, 0, b.host_owner, true).await.unwrap());
    let mut ball = b.f.world.ball(b.f.system, host).await.unwrap();
    ball.type_id = b.kind;
    assert_ne!(ball.type_id, item.type_id);
    replace_ball_preserving_condition(&b, ball).await;
    for method in ["StoreVessel", "BoardStoredShip", "LaunchFromContainer"] {
        let args = if method == "StoreVessel" { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
        b.refuse(method, &args, None).await;
        assert_eq!(b.f.client.outbox.len(), queued);
        selected(&b, old).await;
    }
    let item = row(&b, target).await;
    assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.host_owner, host, FLAG_SHIP_HANGAR));
    assert!(b.f.world.ball(b.f.system, target).await.is_none());
    b.cleanup().await;
}

async fn remove_required_skill(b: &BayFixture) {
    let ty = b.f.sde.types.get(b.kind).unwrap();
    let skill = ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).find_map(|(&id, level)| {
        let skill = ty.attribute(id).unwrap_or(0.0) as i32;
        (skill > 0 && ty.attribute(level).unwrap_or(0.0) > 0.0).then_some(skill)
    }).unwrap();
    let sde = b.f.sde.clone();
    let owner = b.f.owner;
    let world = b.f.world.clone();
    let _market = crate::services::market::lock().await;
    let fences = crate::services::effects::online::capture_online_fences(&world, &[owner]).await;
    b.f.db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let fences = fences.clone();
        Box::pin(async move { crate::services::skills::gm::set_levels(tx, outbox, &sde, &fences, owner, &[(skill, 0)], crate::client::filetime_now()).await })
    }, |(), outbox| async move { crate::services::effects::online::dispatch_changed_world((), outbox, &world) }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn board_stored_refuses_missing_required_skill_without_mutation() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let host = b.host;
    let old = b.f.ship;
    let target = b.stored(b.host_owner).await;
    remove_required_skill(&b).await;
    let queued = b.f.client.outbox.len();
    b.refuse("BoardStoredShip", &[Value::Int(host), Value::Int(target)], None).await;
    assert_eq!(b.f.client.outbox.len(), queued);
    let item = row(&b, target).await;
    assert_eq!((item.owner_id, item.location_id, item.flag_id), (b.host_owner, host, FLAG_SHIP_HANGAR));
    assert!(b.f.world.ball(b.f.system, target).await.is_none());
    selected(&b, old).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn store_rejects_host_nested_below_active_hull_without_cycle() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let old = b.f.ship;
    let host = b.host;
    let host_item = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(host)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(&b.f.db, &host_item, old, FLAG_SHIP_HANGAR, b.host_owner, true).await.unwrap());
    let queued = b.f.client.outbox.len();
    b.refuse("StoreVessel", &[Value::Int(host)], None).await;
    assert_eq!(b.f.client.outbox.len(), queued);
    let host_after = row(&b, host).await;
    let old_after = row(&b, old).await;
    assert_eq!((host_after.owner_id, host_after.location_id, host_after.flag_id), (b.host_owner, old, FLAG_SHIP_HANGAR));
    assert_eq!((old_after.owner_id, old_after.location_id, old_after.flag_id), (b.f.owner, b.f.system, 0));
    selected(&b, old).await;
    b.cleanup().await;
}
