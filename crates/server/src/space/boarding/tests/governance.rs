//! Real committed-dock pending-kick completion and recovery regressions.
use super::*;
use crate::dao::items::{creation, space_ships::FreeShip};
use crate::services::ship::balls;

struct Fixture {
    db: Db,
    world: Arc<World>,
    account: i64,
    owner: i64,
    issuer_account: i64,
    issuer: i64,
    corp: i64,
    ship: i64,
    system: i64,
    station: i64,
}

impl Fixture {
    async fn new() -> Self {
        crate::tests::init_config();
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let station = -2;
        let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
        let (issuer_account, issuer, corp, account, owner, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
            let issuer_account: i64 = tx.query_one("INSERT INTO game.accounts(user_name,password_hash,role) VALUES(nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
            let issuer: i64 = tx.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id) VALUES($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,60000001) RETURNING character_id", &[&issuer_account]).await?.get("character_id");
            let corp: i64 = tx.query_one("INSERT INTO game.corporations(name,ticker,ceo_id,creator_id,station_id,created) VALUES ('dock-completion-'||nextval('game.corporations_corporation_id_seq')::text,'DC'||currval('game.corporations_corporation_id_seq')::text,$1,$1,60000001,1) RETURNING corporation_id", &[&issuer]).await?.get("corporation_id");
            tx.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$2 WHERE character_id=$3", &[&corp, &infinity_core::consts::ROLE_DIRECTOR, &issuer]).await?;
            let account: i64 = tx.query_one("INSERT INTO game.accounts(user_name,password_hash,role) VALUES(nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
            let owner: i64 = tx.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES($1,nextval('game.characters_character_id_seq')::text,1,1,1,$2,$3) RETURNING character_id", &[&account, &corp, &system]).await?.get("character_id");
            let ship = creation::positioned(tx, kind, owner, system, [0.0; 3]).await?;
            tx.execute("UPDATE game.characters SET ship_id=$2,corp_roles=0,corp_roles_hq=0,corp_roles_base=0,corp_roles_other=0,corp_roles_removed_at=NULL WHERE character_id=$1", &[&owner, &ship]).await?;
            let now = crate::client::filetime_now();
            let pending = crate::dao::corporations::pending_kicks::PendingKick { corporation_id: corp, character_id: owner, issuer_id: issuer, requested_at: now, eligible_at: Some(now), reason: 1 };
            crate::dao::corporations::pending_kicks::upsert(tx, &pending).await?;
            Ok::<_, crate::db::Error>((issuer_account, issuer, corp, account, owner, ship))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
        let world = World::new(db.clone(), sde.clone(), 1);
        world.system(system).await.unwrap();
        world.remove_ball(system, ship).await;
        let row = FreeShip { item_id: ship, type_id: kind, owner_id: owner, corporation_id: corp, alliance_id: -1, pos: [0.0; 3] };
        let prepared = balls::piloted(&db, sde, &row, owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        Self { db, world, account, owner, issuer_account, issuer, corp, ship, system, station }
    }

    async fn transition(&self) -> CombatTransition {
        let ship = self.ship;
        self.world.with(self.system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap()
    }

    async fn counts(&self) -> (i64, i64, i64, bool) {
        let corp = self.db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&self.owner]).await.unwrap().get("corporation_id");
        let notices = self.db.query_one("SELECT count(*) AS n FROM game.notifications WHERE receiver_id=$1 AND type_id=92", &[&self.owner]).await.unwrap().get("n");
        let events = self.db.query_one("SELECT count(*) AS n FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2", &[&self.corp, &self.owner]).await.unwrap().get("n");
        let pending = self.db.query_opt("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&self.corp, &self.owner]).await.unwrap().is_some();
        (corp, notices, events, pending)
    }

    async fn cleanup(&self) {
        self.world.remove_ball(self.system, self.ship).await;
        let (account, owner, issuer_account, issuer, corp) = (self.account, self.owner, self.issuer_account, self.issuer, self.corp);
        self.db.transaction(|tx, _: &mut ()| Box::pin(async move {
            crate::services::tests::fixture::cleanup_progress(tx, &[account, owner, issuer_account, issuer, corp], &[owner, issuer]).await
        }), |(), ()| async { Ok(()) }).await.unwrap();
        self.db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1)", &[&&[owner, issuer][..]]).await.unwrap();
        self.db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{owner}")]).await.unwrap();
        self.db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &owner]).await.unwrap();
        self.db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&self.ship]).await.unwrap();
        self.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&self.ship]).await.unwrap();
        self.db.execute("DELETE FROM game.characters WHERE character_id=ANY($1)", &[&&[owner, issuer][..]]).await.unwrap();
        self.db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
        self.db.execute("DELETE FROM game.accounts WHERE user_id=ANY($1)", &[&&[account, issuer_account][..]]).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock; run alone"]
async fn committed_dock_completes_kick_after_caller_cancellation() {
    let f = Fixture::new().await;
    let config = crate::config::tests::test_database();
    let (mut client, connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    let connection = tokio::spawn(async move { let _ = connection.await; });
    let tx = client.transaction().await.unwrap();
    let blocker: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
    tx.query_one("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2 FOR UPDATE", &[&f.corp, &f.owner]).await.unwrap();
    let market = crate::services::market::lock().await;
    let gate = f.world.lock_condition_snapshot(f.system).await.unwrap();
    let transition = f.transition().await;
    let (world, db, owner, ship, system, station) = (f.world.clone(), f.db.clone(), f.owner, f.ship, f.system, f.station);
    let caller = tokio::spawn(async move { world.checkpoint_dock(db, owner, ship, system, station, transition, market, gate).await });
    let result = crate::tests::catch_scenario(async {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let blocked: bool = f.db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await.unwrap().get("blocked");
                let location: i64 = f.db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap().get("location_id");
                let gone = f.world.with(system, move |s| s.ball(ship).is_none()).await.unwrap();
                if blocked && location == station && gone { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("committed dock did not reach the blocked kick");
    }).await;
    caller.abort();
    let _ = caller.await;
    tx.rollback().await.unwrap();
    drop(client);
    let _ = connection.await;
    if result.is_ok() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let (corp, notices, events, pending) = f.counts().await;
                if corp != f.corp && notices == 1 && events == 1 && !pending { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("owned completion did not execute the pending kick");
    }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock; run alone"]
async fn reconciled_dock_kicks_once_and_rollback_does_not_kick() {
    let f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let transition = f.transition().await;
        let (token, generation) = (transition.entries[0].0, transition.entries[0].1.generation);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let plan = Arc::new(DockSettlement { world: Arc::downgrade(&f.world), db: f.db.clone(), character: f.owner,
            ship: f.ship, system: f.system, station: f.station, token, generation, facts: transition.facts.clone(), expected: None, receipt: Mutex::new(Some(sender)) });
        plan.reconcile().await;
        assert!(receiver.await.unwrap().is_err());
        drop(transition);
        f.db.execute("ALTER TABLE game.characters ADD CONSTRAINT test_dock_governance_rollback CHECK (station_id IS DISTINCT FROM -2) NOT VALID", &[]).await.unwrap();
        let market = crate::services::market::lock().await;
        let gate = f.world.lock_condition_snapshot(f.system).await.unwrap();
        let transition = f.transition().await;
        let failed = f.world.checkpoint_dock(f.db.clone(), f.owner, f.ship, f.system, f.station, transition, market, gate).await;
        f.db.execute("ALTER TABLE game.characters DROP CONSTRAINT test_dock_governance_rollback", &[]).await.unwrap();
        assert!(failed.is_err());
        assert_eq!(f.counts().await, (f.corp, 0, 0, true));
        let ship = f.ship;
        assert_eq!(f.world.with(f.system, move |s| s.combat_phase(ship)).await.unwrap(), Some(crate::space::combat::CombatPhase::Live));
        assert_eq!(f.db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap().get::<_, i64>("location_id"), f.system);
        let transition = f.transition().await;
        let (token, generation) = (transition.entries[0].0, transition.entries[0].1.generation);
        let (owner, ship, station, system) = (f.owner, f.ship, f.station, f.system);
        f.db.transaction(|tx, _: &mut ()| Box::pin(async move {
            crate::dao::items::ship::dock(tx, ship, station, infinity_core::consts::FLAG_HANGAR).await?;
            crate::dao::characters::ship::dock(tx, owner, station, system).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async { Ok(()) }).await.unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let plan = Arc::new(DockSettlement { world: Arc::downgrade(&f.world), db: f.db.clone(), character: owner,
            ship, system, station, token, generation, facts: transition.facts.clone(), expected: None, receipt: Mutex::new(Some(sender)) });
        plan.clone().reconcile().await;
        assert!(receiver.await.unwrap().is_ok());
        let counts = f.counts().await;
        assert_ne!(counts.0, f.corp);
        assert_eq!((counts.1, counts.2, counts.3), (1, 1, false));
        plan.reconcile().await;
        crate::services::corps::pending_kicks::trigger(&f.db, &f.world, owner).await.unwrap();
        assert_eq!(f.counts().await, counts);
        assert!(f.world.with(system, move |s| s.ball(ship).is_none()).await.unwrap());
        drop(transition);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
