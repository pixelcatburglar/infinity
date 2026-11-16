//! Shared fleet-warp RPC fixture: real accounts, capsule ships and a live system
//! actor behind the real service dispatch. Members need no client of their own;
//! the commander's client drives requests and its world installs every ship.
use super::*;
use crate::client::Client;
use crate::db::Db;
use crate::services::tests::{client, fixture};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// evefleet/const.py Fleet Formations and Fleet Coordination type IDs.
pub(super) const FORMATION_SKILL: i32 = 57317;
pub(super) const COORDINATION_SKILL: i32 = 57318;

pub(super) fn settings(kind: i64, spacing: i64, size: i64) -> Value {
    Value::dict([("formationType", Value::Int(kind)), ("formationSpacing", Value::Int(spacing)), ("formationSize", Value::Int(size))])
}
pub(super) fn kwargs(fleet: bool, wire: Option<Value>) -> Value {
    let mut entries = vec![("fleet", Value::Bool(fleet)), ("minRange", Value::Float(0.0))];
    if let Some(wire) = wire { entries.push(("fleetFormationSettings", wire)); }
    Value::dict(entries)
}
pub(super) async fn db() -> Db {
    crate::tests::init_config();
    crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap()
}
pub(super) async fn train(db: &Db, character: i64, formation: i32, coordination: i32) {
    crate::dao::skills::training::set_level(db, character, FORMATION_SKILL, formation, 0).await.unwrap();
    crate::dao::skills::training::set_level(db, character, COORDINATION_SKILL, coordination, 0).await.unwrap();
}
pub(super) async fn dispatch(c: &mut Client, service: &str, method: &str, args: &[Value], kwargs: &Value, bind: Option<&Value>) -> CallResult {
    crate::services::dispatch(c, &Call { service, method, args, kwargs, bound: bind.is_some(), bind_params: bind }).await
}
pub(super) fn user_error(result: CallResult, text: &str) {
    let Err(CallError::User { msg, dict }) = result else { panic!("expected user error, got {result:?}") };
    assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".to_string(), Value::unicode(text))]);
}
pub(super) fn fleet_warps(notify: &mut UnboundedReceiver<(String, Vec<Value>)>) -> usize {
    let mut count = 0;
    while let Ok((_, args)) = notify.try_recv() {
        if args.first().and_then(|v| v.as_text()).as_deref() == Some("FleetWarp") { count += 1; }
    }
    count
}
async fn install(db: &Db, world: &Arc<space::World>, system: i64, character: i64, pos: space::V3) -> i64 {
    let ship = crate::dao::items::creation::positioned(db, infinity_core::consts::TYPE_CAPSULE, character, system, pos).await.unwrap();
    let row = crate::dao::items::space_ships::FreeShip {
        item_id: ship, type_id: infinity_core::consts::TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: -1, pos };
    let prepared = crate::services::ship::balls::piloted(db, world.sde().clone(), &row, character, 0.0).await.unwrap();
    world.with(system, move |s| prepared.install(s, |s, b| s.add_ship(b))).await.unwrap();
    ship
}

pub(super) struct Pilot {
    pub fixture: fixture::Fixture,
    pub ship: i64,
    pub notify: UnboundedReceiver<(String, Vec<Value>)>,
}
pub(super) struct FleetWarp {
    pub client: Client,
    _peer: TcpStream,
    pub system: i64,
    pub commander: Pilot,
    pub members: Vec<Pilot>,
}

impl FleetWarp {
    /// Pilots sit 10 km apart on +X from `origin`, the commander at `origin`.
    pub(super) async fn new(db: &Db, system: i64, member_count: usize, origin: space::V3) -> Self {
        let commander_f = fixture::create(db).await.unwrap();
        let (mut client, peer) = client(db.clone(), commander_f.character).await;
        client.login.user_id = commander_f.account;
        let world = client.world.clone();
        world.system(system).await.unwrap();
        let mut pilots = Vec::with_capacity(member_count + 1);
        for index in 0..=member_count {
            let f = if index == 0 { commander_f } else { fixture::create(db).await.unwrap() };
            let pos = [origin[0] + index as f64 * 10_000.0, origin[1], origin[2]];
            let ship = install(db, &world, system, f.character, pos).await;
            let (tx, notify) = unbounded_channel();
            world.register_pilot(f.character, tx);
            pilots.push(Pilot { fixture: f, ship, notify });
        }
        let mut pilots = pilots.into_iter();
        let commander = pilots.next().unwrap();
        let members: Vec<_> = pilots.collect();
        let fleet_id = commander.fixture.character;
        {
            let mut st = state();
            let mut fleet = Fleet::new(fleet_id, vec![]);
            fleet.members.insert(fleet_id, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 1));
            for m in &members { fleet.members.insert(m.fixture.character, Member::new(NONE_ID, NONE_ID, ROLE_MEMBER, JOB_NONE, 1)); }
            st.fleets.insert(fleet_id, fleet);
            st.of.insert(fleet_id, fleet_id);
            for m in &members { st.of.insert(m.fixture.character, fleet_id); }
        }
        client.session.apply(vec![("charid", Value::Int(commander.fixture.character)), ("shipid", Value::Int(commander.ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        Self { client, _peer: peer, system, commander, members }
    }
    fn pilots(&self) -> impl Iterator<Item = &Pilot> {
        self.members.iter().chain(std::iter::once(&self.commander))
    }
    pub(super) async fn settle(&self) { assert!(self.client.world.after(self.system, 1200).await); }
    pub(super) async fn warped(&self, ship: i64) -> Option<(space::V3, f64)> {
        self.client.world.ball(self.system, ship).await
            .filter(|b| b.mode == space::MODE_WARP).map(|b| (b.goto, b.warp.min_range))
    }
    /// Refused requests must neither enqueue a warp nor notify any pilot.
    pub(super) async fn assert_no_warp(&mut self) {
        self.settle().await;
        for pilot in self.pilots() {
            assert_eq!(self.client.world.ball(self.system, pilot.ship).await.unwrap().mode, space::MODE_STOP);
        }
        assert_eq!(fleet_warps(&mut self.commander.notify), 0);
        for member in &mut self.members { assert_eq!(fleet_warps(&mut member.notify), 0); }
    }
    pub(super) async fn finish(self) {
        {
            let mut st = state();
            st.fleets.remove(&self.commander.fixture.character);
            for p in self.pilots() { st.of.remove(&p.fixture.character); }
        }
        for pilot in self.pilots() { self.client.world.remove_ball(self.system, pilot.ship).await; }
        let chars: Vec<i64> = self.pilots().map(|p| p.fixture.character).collect();
        self.client.db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id = ANY($1))", &[&chars]).await.unwrap();
        for p in self.pilots() { fixture::cleanup(&self.client.db, p.fixture).await.unwrap(); }
    }
}
