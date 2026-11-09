use std::{collections::{BTreeMap, HashSet}, sync::Arc};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{client::Client, config::Config, conn::Conn, db::{Db, outbox::Outbox}, handshake::Login, sde::Sde, services::{Call, CallResult}, space::{Ball, WarpState, World, MODE_STOP}};

pub(crate) struct Fixture {
    pub db: Db,
    pub sde: Arc<Sde>,
    pub world: Arc<World>,
    pub client: Client,
    pub owner: i64,
    pub account: i64,
    pub ship: i64,
    pub system: i64,
    _peer: tokio::net::TcpStream,
    before: Option<Vec<(String, Vec<String>)>>,
    baseline_progress_owners: HashSet<i64>,
}

/// Sorted per-row hashes retain duplicates and therefore the row count without
/// constructing a whole-table PostgreSQL aggregate.
pub(crate) async fn table_fingerprint(db: &Db, table: &str) -> Result<Vec<String>, crate::db::Error> {
    let quoted = table.replace('"', "\"\"");
    let sql = format!("SELECT md5(row_to_json(t)::text) AS fingerprint FROM game.\"{quoted}\" t ORDER BY fingerprint");
    Ok(db.query(&sql, &[]).await?.into_iter().map(|row| row.get("fingerprint")).collect())
}

async fn fingerprint(db: &Db) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    for row in db.query("SELECT tablename FROM pg_catalog.pg_tables WHERE schemaname = 'game' ORDER BY tablename", &[]).await.unwrap() {
        let name: String = row.get("tablename");
        let hash = table_fingerprint(db, &name).await.unwrap();
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
        let fences = crate::services::effects::online::capture_online_fences(&self.world, &[owner]).await;
        self.db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let levels = levels.clone();
            let sde = self.sde.clone();
            let fences = fences.clone();
            Box::pin(async move { crate::services::skills::gm::set_levels(tx, outbox, &sde, &fences, owner, &levels, now).await })
        }, |value, outbox| async move {
            let result = crate::services::effects::online::dispatch_changed_world(value, outbox, &self.world);
            crate::scheduler::wake();
            result
        }).await.ok().expect("train drone skills");
    }

    pub(crate) async fn add_pilot(&mut self) -> (i64, i64) {
        self.add_pilot_with_signature(None).await
    }

    pub(in crate::services) async fn add_pilot_with_signature(&mut self, signature: Option<f64>) -> (i64, i64) {
        let account = self.account;
        let system = self.system;
        let ship_type = self.world.ball(system, self.ship).await.unwrap().type_id;
        let baseline_progress_owners = self.baseline_progress_owners.clone();
        let (owner, ship) = self.db.transaction(|tx, _: &mut ()| {
            let baseline_progress_owners = baseline_progress_owners.clone();
            Box::pin(async move {
            let owner = loop {
                let id: i64 = tx.query_one("SELECT nextval('game.characters_character_id_seq') AS id", &[]).await?.get("id");
                if !baseline_progress_owners.contains(&id) { break id; }
            };
            tx.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES ($1,$2,'drone-acceptance-' || gen_random_uuid()::text,1,1,1,1,$3)", &[&owner, &account, &system]).await?;
            let ship = crate::dao::items::creation::positioned(tx, ship_type, owner, system, [0.0; 3]).await?;
            crate::dao::characters::selection::set_ship(tx, owner, ship).await?;
            Ok::<_, crate::db::Error>((owner, ship))
            })
        }, |result, ()| async move { Ok(result) }).await.unwrap();
        if let Some(signature) = signature {
            self.set(ship, ATTR_SIGNATURE_RADIUS, signature).await;
        }
        let mut ball = self.world.ball(system, self.ship).await.unwrap();
        ball.id = ship;
        ball.owner_id = owner;
        ball.char_id = Some(owner);
        if signature.is_some() { ball.signature_radius = signature; }
        self.world.add_ship(system, ball).await.unwrap();
        let fit = crate::services::fit::Fit::load(&self.db, self.sde.clone(), owner, ship).await.ok().expect("load pilot fit");
        let condition = crate::services::combat::prepare_condition_with_fit(&self.db, ship, &fit).await.ok().expect("load pilot condition");
        self.world.with(system, move |s| s.install_condition(ship, condition.into_condition(), fit)).await.expect("pilot system");
        (owner, ship)
    }

    pub(in crate::services) async fn new() -> Self {
        Self::create(false, &[]).await
    }

    pub(in crate::services) async fn with_hold_attributes(attributes: &[i32]) -> Self {
        Self::create(false, attributes).await
    }

    pub(crate) async fn real_system() -> Self {
        Self::create(true, &[]).await
    }

    async fn create(real_system: bool, required_hold_attributes: &[i32]) -> Self {
        crate::tests::init_config();
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = Arc::new(Sde::load(&db).await.unwrap());
        // Startup can rewind identity sequences past deleted characters whose journal rows remain.
        let mut baseline_progress_owners = HashSet::new();
        for row in db.query("SELECT subject_character_id, payload->'WalletChanged'->>'owner_id' AS wallet_owner FROM game.progress_events", &[]).await.unwrap() {
            if let Some(id) = row.get::<_, Option<i64>>("subject_character_id") { baseline_progress_owners.insert(id); }
            if let Some(id) = row.get::<_, Option<String>>("wallet_owner").and_then(|id| id.parse().ok()) { baseline_progress_owners.insert(id); }
        }
        let before = if real_system { None } else { Some(fingerprint(&db).await) };
        let selected_system = real_system.then(|| sde.map.systems()
            .filter(|s| s.security.is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold))
            .min_by(|a, b| a.security.unwrap().total_cmp(&b.security.unwrap()))
            .expect("SDE high-security system").solar_system_id);
        let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
            && required_hold_attributes.iter().all(|attr| t.attribute(*attr).is_some_and(|v| v.is_finite() && v > 0.0))
            && (!real_system || [ATTR_SIGNATURE_RADIUS, ATTR_AGILITY, ATTR_MAX_VELOCITY, ATTR_MASS].iter()
                .all(|attr| t.fit_attribute(*attr).is_some_and(|v| v.is_finite() && v > 0.0))))
            .expect("published ship with valid dogma").type_id;
        assert!(!db.query_one("SELECT EXISTS (SELECT 1 FROM game.corporations WHERE corporation_id=1 AND NOT deleted AND tax_rate > 0)", &[]).await.unwrap().get::<_, bool>("exists"), "fixture corporation must not collect bounty tax");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let baseline_owners = baseline_progress_owners.clone();
        let (account, owner, system, ship) = db.transaction(|tx, _: &mut ()| {
            let baseline_owners = baseline_owners.clone();
            Box::pin(async move {
            let account = loop {
                let id: i64 = tx.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await?.get("id");
                if !baseline_owners.contains(&id) { break id; }
            };
            tx.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,'drone-acceptance-' || gen_random_uuid()::text,$2,0)", &[&account, &&b"fixture"[..]]).await?;
            let owner = loop {
                let id: i64 = tx.query_one("SELECT nextval('game.characters_character_id_seq') AS id", &[]).await?.get("id");
                if !baseline_owners.contains(&id) { break id; }
            };
            tx.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,'drone-acceptance-' || gen_random_uuid()::text,1,1,1,1)", &[&owner, &account]).await?;
            let system = selected_system.unwrap_or(1_000_000_000_000_i64 + owner);
            tx.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&owner, &system]).await?;
            let ship = crate::dao::items::creation::positioned(tx, ship_type, owner, system, [0.0; 3]).await?;
            crate::dao::characters::selection::set_ship(tx, owner, ship).await?;
            Ok::<_, crate::db::Error>((account, owner, system, ship))
            })
        }, |result, ()| async move { Ok(result) }).await.unwrap();
        let world = World::new(db.clone(), sde.clone(), 1);
        let ball = Ball { id: ship, type_id: ship_type, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: Some(owner), signature_radius: None,
            mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
            pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
            goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
            warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
            cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
        world.add_ship(system, ball).await.unwrap();
        let fit = crate::services::fit::Fit::load(&db, sde.clone(), owner, ship).await.ok().expect("load pilot fit");
        let condition = crate::services::combat::prepare_condition_with_fit(&db, ship, &fit).await.ok().expect("load pilot condition");
        world.with(system, move |s| s.install_condition(ship, condition.into_condition(), fit)).await.expect("pilot system");
        let cfg = Arc::new(Config::default());
        let login = Login { user_id: account, user_name: "drone-acceptance".into(), role: 0, client_id: owner, session_id: owner, language_id: "EN".into() };
        let mut client = Client::new(Conn::new(stream, address), cfg.clone(), db.clone(), sde.clone(), crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone()), world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(owner)), ("corpid", Value::Int(1)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system))]);
        world.register_pilot(owner, client.notify_tx.clone());
        Self { db, sde, world, client, owner, account, ship, system, _peer: peer, before, baseline_progress_owners }
    }

    pub(in crate::services) async fn drone(&self, type_id: i32, count: i32, singleton: bool) -> i64 {
        self.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,$6) RETURNING item_id", &[&type_id, &self.owner, &self.ship, &FLAG_DRONE_BAY, &i64::from(count), &singleton]).await.unwrap().get("item_id")
    }

    pub(in crate::services) async fn set(&self, item: i64, attr: i32, value: f64) {
        assert!(crate::dao::items::dogma::set_override(&self.db, item, attr, value).await.unwrap(), "override requires a real singleton item");
    }

    pub(crate) async fn cleanup(self) {
        let owners: Vec<i64> = self.db.query("SELECT character_id FROM game.characters WHERE user_id=$1", &[&self.account]).await.unwrap().iter().map(|row| row.get("character_id")).collect();
        for &owner in &owners { crate::services::fleets::lifecycle::leave(&self.world, owner); }
        let fixture_owners = owners.clone();
        let all_balls = self.before.is_some();
        let (ids, owned_drones): (Vec<i64>, Vec<(i64, String)>) = self.world.with(self.system, move |s| {
            let ids = s.balls().filter(|b| all_balls || fixture_owners.contains(&b.owner_id)).map(|b| b.id).collect();
            let drones = s.balls().filter_map(|b| {
                let drone = s.drone(b.id)?;
                if !fixture_owners.contains(&drone.owner) { return None; }
                let entity = s.combat_entity(b.id)?;
                Some((entity.item_id, serde_json::to_string(&entity.incarnation).unwrap()))
            }).collect();
            (ids, drones)
        }).await.unwrap();
        for id in ids { self.world.remove_ball(self.system, id).await; crate::services::combat::forget_gm_spawn(&self.world, id); crate::services::combat::forget_ship(id); }
        crate::services::combat::reset(&self.client);
        for &owner in &owners { self.world.unobserve(owner); self.world.unregister_pilot_generation(owner, None); }
        self.world.flush_positions().await.unwrap();
        let mut owner_ids = owners.clone();
        owner_ids.push(self.account);
        let subject_ids = owners.clone();
        self.db.transaction(|tx, _: &mut ()| {
            let owner_ids = owner_ids.clone();
            let subject_ids = subject_ids.clone();
            let owned_drones = owned_drones.clone();
            Box::pin(async move {
                for (item, incarnation) in &owned_drones {
                    let rows = tx.query("SELECT event_id::text AS id FROM game.progress_events WHERE subject_character_id IS NULL AND kind IN ('DamageApplied', 'RemoteRepairApplied', 'WarpScrambleApplied') AND (payload->kind->'target'->>'item_id')::bigint=$1 AND payload->kind->'target'->'incarnation'=$2::text::jsonb", &[item, incarnation]).await?;
                    let ids: Vec<String> = rows.iter().map(|row| row.get("id")).collect();
                    tx.execute("DELETE FROM game.progress_deliveries WHERE event_id::text=ANY($1)", &[&ids]).await?;
                    let deleted = tx.execute("DELETE FROM game.progress_events WHERE event_id::text=ANY($1)", &[&ids]).await?;
                    assert_eq!(deleted as usize, ids.len(), "captured owned-drone events were not all removed");
                }
                crate::services::tests::fixture::cleanup_progress(tx, &owner_ids, &subject_ids).await
            })
        }, |(), ()| async move { Ok(()) }).await.unwrap();
        if self.before.is_some() {
            self.db.execute("DELETE FROM game.map_events WHERE solar_system_id=$1", &[&self.system]).await.unwrap();
        }
        if self.before.is_none() {
            let kills: Vec<i64> = self.db.query("SELECT kill_id FROM game.killmails WHERE victim_id=ANY($1)", &[&owners]).await.unwrap().iter().map(|r| r.get("kill_id")).collect();
            self.db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id=ANY($2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &kills]).await.unwrap();
            for kill in &kills {
                let job = format!("{}:{kill}", crate::services::bounty::payout::KIND);
                self.db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap();
                self.db.execute("DELETE FROM game.bounty_payouts WHERE killmail_id=$1", &[&kill]).await.unwrap();
            }
            let messages: Vec<i64> = self.db.query("SELECT DISTINCT m.message_id FROM game.mail_messages m LEFT JOIN game.mail_status s ON s.message_id=m.message_id WHERE m.sender_id=ANY($1) OR s.char_id=ANY($1)", &[&owners]).await.unwrap().iter().map(|r| r.get("message_id")).collect();
            self.db.execute("DELETE FROM game.mail_status WHERE message_id=ANY($1)", &[&messages]).await.unwrap();
            self.db.execute("DELETE FROM game.mail_messages WHERE message_id=ANY($1)", &[&messages]).await.unwrap();
            self.db.execute("DELETE FROM game.insurance WHERE owner_id=ANY($1)", &[&owners]).await.unwrap();
            self.db.execute("DELETE FROM game.killmails WHERE victim_id=ANY($1)", &[&owners]).await.unwrap();
        }
        for owner in owners {
            self.db.execute("DELETE FROM game.jobs WHERE job_id IN (SELECT 'kill-right-expiry:' || kill_right_id::text FROM game.kill_rights WHERE from_id=$1 OR to_id=$1)", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.kill_rights WHERE from_id=$1 OR to_id=$1", &[&owner]).await.unwrap();
            self.db.execute("DELETE FROM game.notifications WHERE receiver_id=$1", &[&owner]).await.unwrap();
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
        if let Some(before) = self.before {
            let after = fingerprint(&self.db).await;
            if after != before {
                let before: BTreeMap<_, _> = before.into_iter().collect();
                let after: BTreeMap<_, _> = after.into_iter().collect();
                let mut names: Vec<_> = before.keys().chain(after.keys()).collect();
                names.sort();
                names.dedup();
                let differences: Vec<_> = names.into_iter().filter_map(|name| {
                    let old = before.get(name);
                    let new = after.get(name);
                    if old == new { return None; }
                    let mut counts = BTreeMap::<&str, i64>::new();
                    for hash in old.into_iter().flatten() { *counts.entry(hash).or_default() -= 1; }
                    for hash in new.into_iter().flatten() { *counts.entry(hash).or_default() += 1; }
                    let removed: i64 = counts.values().filter(|&&n| n < 0).map(|n| -n).sum();
                    let added: i64 = counts.values().filter(|&&n| n > 0).sum();
                    Some((name, old.map(Vec::len), new.map(Vec::len), removed, added))
                }).collect();
                panic!("database fingerprint mismatch (table, before rows, after rows, removed hashes, added hashes): {differences:?}");
            }
        }
    }
}
