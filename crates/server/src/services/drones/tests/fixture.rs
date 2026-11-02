use std::{collections::BTreeMap, sync::Arc};
use marshal::Value;
use crate::{client::Client, config::Config, conn::Conn, consts::{*, dogma::*}, db::{Db, outbox::Outbox}, handshake::Login, sde::Sde, services::{Call, CallResult}, space::{Ball, WarpState, World, MODE_STOP}};

pub(in crate::services) struct Fixture {
    pub db: Db,
    pub sde: Arc<Sde>,
    pub world: Arc<World>,
    pub client: Client,
    pub owner: i64,
    pub account: i64,
    pub ship: i64,
    pub system: i64,
    _peer: tokio::net::TcpStream,
    before: Vec<(String, String)>,
}

async fn fingerprint(db: &Db) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for row in db.query("SELECT tablename FROM pg_catalog.pg_tables WHERE schemaname = 'game' ORDER BY tablename", &[]).await.unwrap() {
        let name: String = row.get("tablename");
        let quoted = name.replace('"', "\"\"");
        let sql = format!("SELECT count(*)::text || ':' || md5(COALESCE(string_agg(md5(row_to_json(t)::text), '' ORDER BY md5(row_to_json(t)::text)), '')) AS fingerprint FROM game.\"{quoted}\" t");
        let hash = db.query_one(&sql, &[]).await.unwrap().get("fingerprint");
        result.push((name, hash));
    }
    result
}

impl Fixture {
    pub(in crate::services) async fn call(&mut self, service: &str, method: &str, args: &[Value]) -> CallResult {
        let params = if service == "ship" { Value::tuple([Value::Int(self.system), Value::Int(5)]) } else { Value::Int(self.system) };
        let kwargs = Value::None;
        let call = Call { service, method, args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
        if service == "ship" { crate::services::ship::dispatch(&mut self.client, &call).await.unwrap() }
        else { crate::services::drones::dispatch(&mut self.client, &call).await.unwrap() }
    }

    pub(in crate::services) async fn train_drone(&self, type_id: i32) {
        let drone = self.sde.types.get(type_id).unwrap();
        let skill = self.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && t.effects().iter().any(|e| e.effect_id == 316)).expect("Drones skill effect").type_id;
        let mut levels = BTreeMap::from([(skill, 5)]);
        for (&skill_attr, level_attr) in ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS) {
            let id = drone.attribute(skill_attr).unwrap_or(0.0) as i32;
            let level = drone.attribute(level_attr).unwrap_or(0.0) as i32;
            if id > 0 { levels.entry(id).and_modify(|v| *v = (*v).max(level)).or_insert(level); }
        }
        let levels: Vec<_> = levels.into_iter().collect();
        let now = crate::client::filetime_now();
        let owner = self.owner;
        let _lock = crate::services::market::lock().await;
        self.db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let levels = levels.clone();
            let sde = self.sde.clone();
            Box::pin(async move { crate::services::skills::gm::set_levels(tx, outbox, &sde, owner, &levels, now).await })
        }, |value, outbox| async move {
            let result = crate::services::effects::online::dispatch_changed_world(value, outbox, &self.world);
            crate::scheduler::wake();
            result
        }).await.ok().expect("train drone skills");
    }

    pub(in crate::services) async fn add_pilot(&mut self) -> (i64, i64) {
        let account = self.account;
        let system = self.system;
        let ship_type = self.world.ball(system, self.ship).await.unwrap().type_id;
        let (owner, ship) = self.db.transaction(|tx, _: &mut ()| Box::pin(async move {
            let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES ($1,'drone-acceptance-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,$2) RETURNING character_id", &[&account, &system]).await?.get("character_id");
            let ship = crate::dao::items::creation::positioned(tx, ship_type, owner, system, [0.0; 3]).await?;
            crate::dao::characters::selection::set_ship(tx, owner, ship).await?;
            Ok::<_, crate::db::Error>((owner, ship))
        }), |result, ()| async move { Ok(result) }).await.unwrap();
        let mut ball = self.world.ball(system, self.ship).await.unwrap();
        ball.id = ship;
        ball.owner_id = owner;
        ball.char_id = Some(owner);
        self.world.add_ship(system, ball).await.unwrap();
        (owner, ship)
    }

    pub(in crate::services) async fn new() -> Self {
        crate::tests::init_config();
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = Arc::new(Sde::load(&db).await.unwrap());
        let before = fingerprint(&db).await;
        let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("published ship").type_id;
        assert!(!db.query_one("SELECT EXISTS (SELECT 1 FROM game.corporations WHERE corporation_id=1 AND NOT deleted AND tax_rate > 0)", &[]).await.unwrap().get::<_, bool>("exists"), "fixture corporation must not collect bounty tax");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let (account, owner, system, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('drone-acceptance-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'drone-acceptance-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
            let system = 1_000_000_000_000_i64 + owner;
            tx.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&owner, &system]).await?;
            let ship = crate::dao::items::creation::positioned(tx, ship_type, owner, system, [0.0; 3]).await?;
            crate::dao::characters::selection::set_ship(tx, owner, ship).await?;
            Ok::<_, crate::db::Error>((account, owner, system, ship))
        }), |result, ()| async move { Ok(result) }).await.unwrap();
        let world = World::new(db.clone(), sde.clone(), 1);
        let ball = Ball { id: ship, type_id: ship_type, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: Some(owner), signature_radius: None,
            mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
            pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
            goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
            warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
            cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
        world.add_ship(system, ball).await.unwrap();
        let cfg = Arc::new(Config::default());
        let login = Login { user_id: account, user_name: "drone-acceptance".into(), role: 0, client_id: owner, session_id: owner, language_id: "EN".into() };
        let mut client = Client::new(Conn::new(stream, address), cfg.clone(), db.clone(), sde.clone(), crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone()), world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(owner)), ("corpid", Value::Int(1)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system))]);
        world.register_pilot(owner, client.notify_tx.clone());
        Self { db, sde, world, client, owner, account, ship, system, _peer: peer, before }
    }

    pub(in crate::services) async fn drone(&self, type_id: i32, count: i32, singleton: bool) -> i64 {
        self.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,$6) RETURNING item_id", &[&type_id, &self.owner, &self.ship, &FLAG_DRONE_BAY, &count, &singleton]).await.unwrap().get("item_id")
    }

    pub(in crate::services) async fn set(&self, item: i64, attr: i32, value: f64) {
        assert!(crate::dao::items::dogma::set_override(&self.db, item, attr, value).await.unwrap(), "override requires a real singleton item");
    }

    pub(in crate::services) async fn cleanup(self) {
        let owners: Vec<i64> = self.db.query("SELECT character_id FROM game.characters WHERE user_id=$1", &[&self.account]).await.unwrap().iter().map(|row| row.get("character_id")).collect();
        for &owner in &owners { crate::services::fleets::lifecycle::leave(&self.world, owner); }
        let ids: Vec<i64> = self.world.with(self.system, |s| s.balls().map(|b| b.id).collect()).await.unwrap();
        for id in ids { self.world.remove_ball(self.system, id).await; crate::services::combat::forget_gm_spawn(&self.world, id); crate::services::combat::forget_ship(id); }
        crate::services::combat::reset(&self.client);
        for &owner in &owners { self.world.unobserve(owner); self.world.unregister_pilot(owner); }
        self.world.flush_positions().await.unwrap();
        for owner in owners {
            self.db.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.weapon_banks WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&owner, &self.account]).await.unwrap();
        }
        self.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&self.account]).await.unwrap();
        assert_eq!(fingerprint(&self.db).await, self.before);
    }
}
