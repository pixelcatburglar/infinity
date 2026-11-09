//! Real-database coverage for the reserved service account, character selection, and monotonic ID sequences on schema replay.

use super::*;
use crate::dao::{accounts::selection as account_selection, characters::selection as character_selection};
use crate::db::transaction::TransactionError;
use std::future::Future;

mod stack_quantities;

#[derive(Debug)]
enum Rollback {
    Database(Error),
    Observed { collisions: Vec<bool>, isolated: bool },
}
impl From<Error> for Rollback {
    fn from(error: Error) -> Self { Self::Database(error) }
}
impl From<tokio_postgres::Error> for Rollback {
    fn from(error: tokio_postgres::Error) -> Self { Self::Database(error.into()) }
}
impl TransactionError for Rollback {
    fn is_retryable_transaction(&self) -> bool {
        match self { Self::Database(error) => error.is_retryable_transaction(), Self::Observed { .. } => false }
    }
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn simulated_account_startup_login_collision_and_selection() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    assert!(simulated::ensure(&db).await.unwrap());
    let reserved = db.query_one("SELECT user_id, role, password_hash, isfinite(created_at) AS finite, last_login::text AS last_login FROM game.accounts WHERE lower(user_name) = lower($1)", &[&simulated::ACCOUNT_NAME]).await.unwrap();
    let reserved_id: i64 = reserved.get("user_id");
    let last_login: Option<String> = reserved.get("last_login");
    assert_ne!(reserved.get::<_, i64>("role") & ROLE_SERVICE, 0);
    assert!(reserved.get::<_, Vec<u8>>("password_hash").is_empty());
    assert!(reserved.get::<_, bool>("finite"));
    account_selection::creation_date(&db, reserved_id, infinity_core::consts::FILETIME_SECOND, infinity_core::consts::FILETIME_UNIX_EPOCH).await.unwrap();
    assert!(simulated::ensure(&db).await.unwrap());
    let again = db.query_one("SELECT user_id FROM game.accounts WHERE lower(user_name) = lower($1)", &[&simulated::ACCOUNT_NAME]).await.unwrap();
    assert_eq!(again.get::<_, i64>("user_id"), reserved_id);

    for enabled in [false, true] {
        let policy = DevelopmentConfig { auto_register_accounts: enabled, ..Default::default() };
        for name in [simulated::ACCOUNT_NAME, "INFINITY-SIMULATED-PILOTS", "InFiNiTy-SiMuLaTeD-PiLoTs"] {
            for password in [&b""[..], &b"fixture"[..]] {
                assert!(db.login(name, password, &policy).await.unwrap().is_none());
            }
        }
    }
    let after: Option<String> = db.query_one("SELECT last_login::text AS last_login FROM game.accounts WHERE user_id = $1", &[&reserved_id]).await.unwrap().get("last_login");
    assert_eq!(last_login, after);
    assert_eq!((ROLE_PLAYER_LOGIN | GM_ROLES) & ROLE_SERVICE, 0);

    let row = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('simulated-test-' || nextval('game.accounts_user_id_seq')::text, $1, $2) RETURNING user_id, user_name", &[&&b"fixture"[..], &ROLE_PLAYER_LOGIN]).await.unwrap();
    let user_id: i64 = row.get("user_id");
    let name: String = row.get("user_name");
    let result: anyhow::Result<()> = async {
        let account = db.login(&name, b"fixture", &DevelopmentConfig::default()).await?;
        anyhow::ensure!(account.is_some_and(|account| account.user_id == user_id), "ordinary login failed");
        db.execute("UPDATE game.accounts SET role = $2 WHERE user_id = $1", &[&user_id, &ROLE_SERVICE]).await?;
        anyhow::ensure!(db.login(&name, b"fixture", &DevelopmentConfig::default()).await?.is_none(), "renamed service account logged in");
        Ok(())
    }.await;
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&user_id]).await.unwrap();
    result.unwrap();

    let outcome: Result<(), Rollback> = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let scratch: String = tx.query_one("SELECT 'simulated-scratch-' || nextval('game.accounts_user_id_seq')::text AS name", &[]).await?.get("name");
        tx.execute("UPDATE game.accounts SET user_name = $2 WHERE user_id = $1", &[&reserved_id, &scratch]).await?;
        let mut collisions = Vec::new();
        for password in [&b""[..], &b"fixture"[..]] {
            let inserted = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ($1, $2, $3) RETURNING user_id", &[&simulated::ACCOUNT_NAME, &password, &ROLE_PLAYER_LOGIN]).await?;
            let id: i64 = inserted.get("user_id");
            let accepted = simulated::ensure(tx).await?;
            let row = tx.query_one("SELECT user_id, password_hash, role FROM game.accounts WHERE user_id = $1", &[&id]).await?;
            collisions.push(!accepted && row.get::<_, i64>("user_id") == id && row.get::<_, Vec<u8>>("password_hash") == password && row.get::<_, i64>("role") == ROLE_PLAYER_LOGIN);
            tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&id]).await?;
        }
        let ordinary: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('simulated-selection-' || nextval('game.accounts_user_id_seq')::text, $1, $2) RETURNING user_id", &[&&b"fixture"[..], &ROLE_PLAYER_LOGIN]).await?.get("user_id");
        let insert_character = "INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, solar_system_id, constellation_id, region_id) VALUES ($1, 'simulated-selection-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, 1, 1, 1) RETURNING character_id";
        let service_character: i64 = tx.query_one(insert_character, &[&reserved_id]).await?.get("character_id");
        let ordinary_character: i64 = tx.query_one(insert_character, &[&ordinary]).await?.get("character_id");
        let characters = character_selection::characters(tx, ordinary, infinity_core::consts::FILETIME_SECOND, infinity_core::consts::FILETIME_UNIX_EPOCH).await.map_err(|e| Error::Progress(e.into()))?;
        let owns = character_selection::owns(tx, service_character, ordinary).await?;
        let selected = character_selection::selected(tx, service_character, ordinary).await?;
        let isolated = characters.iter().any(|character| character.character_id == ordinary_character)
            && characters.iter().all(|character| character.character_id != service_character)
            && !owns && selected.is_none();
        Err(Rollback::Observed { collisions, isolated })
    }), |(), (): ()| async { Ok(()) }).await;
    match outcome {
        Err(Rollback::Observed { collisions, isolated }) => { assert_eq!(collisions, vec![true, true]); assert!(isolated); }
        other => panic!("collision transaction failed: {other:?}"),
    }
    let restored: i64 = db.query_one("SELECT user_id FROM game.accounts WHERE lower(user_name) = lower($1)", &[&simulated::ACCOUNT_NAME]).await.unwrap().get("user_id");
    assert_eq!(restored, reserved_id);
}

#[derive(Clone, Copy)]
enum ReplayFixture { Character, Corporation }

async fn replay_sequence_fixture(db: &Db, user_id: i64, fixture: ReplayFixture) -> anyhow::Result<()> {
    use crate::dao::{characters::creation::{self, NewCharacter}, corporations::registry::{self, NewCorporation}};

    let name = format!("sequence-replay-{user_id}");
    let ticker = format!("SEQ{user_id}");
    let character = NewCharacter {
        user_id, name: &name, race_id: 1, bloodline_id: 1, gender_id: 1,
        ancestry_id: None, school_id: None, corporation_id: 1, station_id: 1,
        solar_system_id: 1, constellation_id: 1, region_id: 1,
        appearance: &[], portrait: &[], balance: 0.0,
    };
    let corporation = NewCorporation {
        name: &name, ticker: &ticker, description: "", url: "", tax: 0.0, lp_tax: 0.0,
        ceo: user_id, station: 1, recruiting: false, friendly: false,
        shapes: [None; 7], now: 1, member_limit: None,
    };
    let (max_sql, delete_sql) = match fixture {
        ReplayFixture::Character => ("SELECT max(character_id) AS max_id FROM game.characters", "DELETE FROM game.characters WHERE character_id = $1"),
        ReplayFixture::Corporation => ("SELECT max(corporation_id) AS max_id FROM game.corporations", "DELETE FROM game.corporations WHERE corporation_id = $1"),
    };
    let mut owned = Vec::new();
    let result: anyhow::Result<()> = async {
        let deleted = match fixture {
            ReplayFixture::Character => creation::create(db, &character).await?,
            ReplayFixture::Corporation => registry::create(db, &corporation).await?,
        };
        owned.push(deleted);
        let maximum: Option<i64> = db.query_one(max_sql, &[]).await?.get("max_id");
        anyhow::ensure!(maximum == Some(deleted), "fixture {deleted} is not the highest row: {maximum:?}");
        anyhow::ensure!(db.execute(delete_sql, &[&deleted]).await? == 1, "fixture was not deleted");
        {
            let client = db.pool.get().await?;
            client.batch_execute(GAME_SCHEMA).await?;
        }
        let recreated = match fixture {
            ReplayFixture::Character => creation::create(db, &character).await?,
            ReplayFixture::Corporation => registry::create(db, &corporation).await?,
        };
        owned.push(recreated);
        anyhow::ensure!(recreated > deleted, "replay reused or moved backwards: {deleted} -> {recreated}");
        Ok(())
    }.await;
    let mut cleanup_error = None;
    for id in owned {
        if let Err(error) = db.execute(delete_sql, &[&id]).await {
            cleanup_error = Some(error);
        }
    }
    if let Some(error) = cleanup_error { return Err(error.into()); }
    result
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database; run this test alone"]
async fn schema_replay_preserves_deleted_character_and_corporation_ids() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let suffix: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let name = format!("sequence-replay-{suffix}");
    let account = authentication::register(&db, &name, b"fixture", ROLE_PLAYER_LOGIN, 0).await.unwrap();
    let result: anyhow::Result<()> = async {
        replay_sequence_fixture(&db, account.user_id, ReplayFixture::Character).await?;
        replay_sequence_fixture(&db, account.user_id, ReplayFixture::Corporation).await?;
        Ok(())
    }.await;
    let cleanup = db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account.user_id]).await;
    cleanup.unwrap();
    result.unwrap();
}

/// Join cancellation of every gate/client task before fixture cleanup.
#[derive(Clone, Default)]
struct RaceReapers(std::sync::Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>);
impl RaceReapers {
    fn cancel<T: Send + 'static>(&self, task: tokio::task::JoinHandle<T>) {
        task.abort();
        let join = tokio::spawn(async move { let _ = task.await; });
        self.0.lock().unwrap().push(join);
    }
    async fn join(&self) {
        let tasks = std::mem::take(&mut *self.0.lock().unwrap());
        for task in tasks {
            tokio::time::timeout(std::time::Duration::from_secs(30), task).await.expect("cancelled task did not join").expect("task reaper failed");
        }
    }
}

/// A real transaction holding the only selector-pool lease, without locking a game row.
struct SelectionLeaseGate {
    release: std::sync::Arc<tokio::sync::Notify>,
    entered: Option<tokio::sync::oneshot::Receiver<()>>,
    task: Option<tokio::task::JoinHandle<Result<(), Error>>>,
    reapers: RaceReapers,
}

impl SelectionLeaseGate {
    fn start(db: Db, reapers: RaceReapers) -> Self {
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let wait = release.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let entered = std::sync::Arc::new(std::sync::Mutex::new(Some(entered_tx)));
        let task = tokio::spawn(async move {
            db.transaction(
                |_tx, _: &mut ()| {
                    let wait = wait.clone();
                    let entered = entered.clone();
                    Box::pin(async move {
                        if let Some(sender) = entered.lock().unwrap().take() { let _ = sender.send(()); }
                        wait.notified().await;
                        Ok::<(), Error>(())
                    })
                },
                |value, ()| async move { Ok(value) },
            ).await
        });
        Self { release, entered: Some(entered_rx), task: Some(task), reapers }
    }

    async fn wait_entered(&mut self) {
        tokio::time::timeout(std::time::Duration::from_secs(30), self.entered.take().unwrap()).await.expect("lease gate did not enter").expect("lease gate stopped");
    }

    async fn finish(mut self) {
        self.release.notify_one();
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), self.task.as_mut().unwrap()).await.expect("lease gate did not finish").expect("lease gate task failed");
        drop(self.task.take());
        result.expect("lease gate transaction failed");
    }
}

impl Drop for SelectionLeaseGate {
    fn drop(&mut self) {
        self.release.notify_one();
        if let Some(task) = self.task.take() { self.reapers.cancel(task); }
    }
}

struct SelectionTask<T: Send + 'static>(Option<tokio::task::JoinHandle<T>>, RaceReapers);
impl<T: Send + 'static> Drop for SelectionTask<T> {
    fn drop(&mut self) { if let Some(task) = self.0.take() { self.1.cancel(task); } }
}
impl<T: Send + 'static> SelectionTask<T> {
    async fn finish(&mut self) -> T {
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), self.0.as_mut().unwrap()).await.expect("client task timed out").expect("client task failed");
        drop(self.0.take());
        result
    }
}

/// Preserve assertion failures while still closing real clients and releasing leases.
async fn catch_race<F: Future>(future: F) -> Result<F::Output, Box<dyn std::any::Any + Send>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
        Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
        Err(error) => std::task::Poll::Ready(Err(error)),
    }).await
}

/// Observe real FIFO waiters; the timeout is a deadlock guard, not an ordering delay.
async fn selection_waiters(db: &Db, wanted: usize, selection: &SelectionTask<(crate::client::Client, crate::services::CallResult)>) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if db.pool.status().waiting >= wanted { break; }
            assert!(!selection.0.as_ref().unwrap().is_finished(), "selection finished before its gated checkout");
            tokio::task::yield_now().await;
        }
    }).await.expect("selector did not queue its checkout");
}

async fn real_kick(client: &mut crate::client::Client, corp: i64, target: i64) {
    let args = [marshal::Value::Int(target)];
    let kwargs = marshal::Value::Dict(vec![]);
    let binding = marshal::Value::Int(corp);
    let call = crate::services::Call { service: "corpRegistry", method: "KickOutMember", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
    let result = crate::services::dispatch(client, &call).await;
    assert!(matches!(result, Ok(marshal::Value::None)), "real kick failed: {result:?}");
}

async fn assert_departure(db: &Db, target: i64, source: i64, marker: i64) -> i64 {
    let row = db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target]).await.unwrap();
    let corporation: i64 = row.get("corporation_id");
    assert_ne!(corporation, source);
    let row = db.query_one("SELECT (SELECT count(*) FROM game.employment_history WHERE char_id=$1 AND corporation_id=$2) AS employment, (SELECT count(*) FROM game.corp_member_events WHERE character_id=$1 AND corporation_id=$2 AND event_type_id=14) AS events, (SELECT count(*) FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$3) AS notices, (SELECT count(*) FROM game.corp_pending_kicks WHERE character_id=$1) AS pending", &[&target, &source, &marker]).await.unwrap();
    assert_eq!(row.get::<_, i64>("events"), 1);
    assert_eq!(row.get::<_, i64>("notices"), 1);
    assert_eq!(row.get::<_, i64>("pending"), 0);
    // The fixture's source membership is seeded directly; departure still records
    // its new NPC employment without inventing a source-history row.
    assert_eq!(row.get::<_, i64>("employment"), 0);
    assert!(db.query_opt("SELECT char_id FROM game.employment_history WHERE char_id=$1 AND corporation_id=$2", &[&target, &corporation]).await.unwrap().is_some());
    corporation
}

async fn read_corporation_change(peer: &mut tokio::net::TcpStream, old: i64, new: i64, old_hq: i64, new_hq: Option<i64>) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let frame = crate::framing::read_frame(peer).await.unwrap().expect("client transport closed before session correction");
            let packet = crate::macho::Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
            if packet.command != crate::macho::cmd::SESSIONCHANGENOTIFICATION { continue; }
            let Some(pair) = packet.body.get(1).and_then(marshal::Value::items) else { continue };
            let Some(changes) = pair.get(1) else { continue };
            let Some(change) = changes.get("corpid") else { continue };
            if *change == marshal::Value::tuple([marshal::Value::Int(old), marshal::Value::Int(new)]) {
                if new_hq != Some(old_hq) {
                    assert_eq!(changes.get("hqID"), Some(&marshal::Value::tuple([marshal::Value::Int(old_hq), new_hq.map(marshal::Value::Int).unwrap_or(marshal::Value::None)])));
                }
                break;
            }
        }
    }).await.expect("real session-change packet was not received");
}

async fn login_kick_order(kick_before_reread: bool) {
    use crate::services::tests::{self, fixture};
    use marshal::Value;
    crate::tests::init_config();
    let mut selector_config = crate::config::tests::test_database();
    selector_config.pool_size = 1;
    selector_config.checkout_timeout_secs = 0;
    let selector_db = Db::connect(&selector_config).await.unwrap();
    let kick_db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&kick_db).await.unwrap();
    let target = fixture::create(&kick_db).await.unwrap();
    let marker: i64 = kick_db.query_one("SELECT COALESCE(MAX(notification_id),0) AS marker FROM game.notifications WHERE receiver_id=ANY($1)", &[&vec![ceo.character,target.character]]).await.unwrap().get("marker");
    let (mut selector, peer) = tests::client(selector_db.clone(), target.character).await;
    let station = selector.sde.map.stations().find(|station| station.solar_system_id.is_some() && station.constellation_id.is_some() && station.region_id.is_some()).expect("SDE station with complete location");
    let (station_id, system, constellation, region) = (station.station_id, station.solar_system_id.unwrap(), station.constellation_id.unwrap(), station.region_id.unwrap());
    let corp: i64 = kick_db.query_one("INSERT INTO game.corporations(name,ticker,ceo_id,creator_id,station_id,created) VALUES ('login-race-' || nextval('game.corporations_corporation_id_seq')::text,'LR' || currval('game.corporations_corporation_id_seq')::text,$1,$1,$2,1) RETURNING corporation_id", &[&ceo.character, &station_id]).await.unwrap().get("corporation_id");
    let people = [ceo.character, target.character];
    kick_db.execute("UPDATE game.characters SET corporation_id=$1,station_id=$2,solar_system_id=$3,constellation_id=$4,region_id=$5 WHERE character_id=ANY($6)", &[&corp, &station_id, &system, &constellation, &region, &people.as_slice()]).await.unwrap();
    kick_db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    selector.login.user_id = target.account;
    selector.session.apply(vec![("charid", Value::None)]);
    let world = selector.world.clone();
    let sde = selector.sde.clone();
    let (mut manager, manager_peer) = tests::client(kick_db.clone(), ceo.character).await;
    manager.world = world.clone();
    manager.chat = selector.chat.clone();
    manager.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(station_id))]);
    let run_db = selector_db.clone();
    let other_db = kick_db.clone();
    let reapers = RaceReapers::default();
    let scenario_reapers = reapers.clone();
    let result = tokio::spawn(async move {
        let mut current = SelectionLeaseGate::start(run_db.clone(), scenario_reapers.clone());
        current.wait_entered().await;
        let mut selection = SelectionTask(Some(tokio::spawn(async move {
            let args = [Value::Int(target.character)];
            let kwargs = Value::Dict(vec![]);
            let call = crate::services::Call { service: "charUnboundMgr", method: "SelectCharacterID", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let result = crate::services::dispatch(&mut selector, &call).await;
            (selector, result)
        })), scenario_reapers.clone());
        let held = loop {
            selection_waiters(&run_db, 1, &selection).await;
            let batch = run_db.pool.status().waiting;
            let mut next = SelectionLeaseGate::start(run_db.clone(), scenario_reapers.clone());
            selection_waiters(&run_db, batch + 1, &selection).await;
            current.finish().await;
            next.wait_entered().await;
            selection_waiters(&run_db, 1, &selection).await;
            if let Some(session) = world.online_session(target.character) {
                assert_eq!(session.corporation_id, Some(corp), "publication must precede the blocked fresh reread");
                assert_eq!(run_db.pool.status().waiting, 1, "fresh reread must wait behind the held selector lease");
                break next;
            }
            current = next;
        };
        if kick_before_reread {
            let kicked = catch_race(async {
                real_kick(&mut manager, corp, target.character).await;
                assert_departure(&other_db, target.character, corp, marker).await
            }).await;
            held.finish().await;
            let (mut client, selected) = selection.finish().await;
            let checked = catch_race(async {
                let new_corp = kicked.unwrap_or_else(|error| std::panic::resume_unwind(error));
                assert!(matches!(selected, Ok(Value::None)), "real selection failed: {selected:?}");
                assert_eq!(client.session.int("corpid"), Some(new_corp));
                assert_eq!(client.session.int("allianceid"), None);
                for key in ["corprole", "rolesAtAll", "rolesAtHQ", "rolesAtBase", "rolesAtOther"] { assert_eq!(client.session.int(key), Some(0)); }
                assert_eq!(client.session.int("hqID"), sde.npc_corporations.get(new_corp).and_then(|row| row.station_id));
                let session = world.online_session(target.character).unwrap();
                assert_eq!((session.corporation_id, session.alliance_id, session.corporation_role), (Some(new_corp), None, 0));
            }).await;
            let cleanup = client.leave_world().await;
            drop(peer);
            cleanup.expect("selected client cleanup failed");
            if let Err(error) = checked { std::panic::resume_unwind(error); }
        } else {
            held.finish().await;
            let (mut client, selected) = selection.finish().await;
            let before = catch_race(async {
                assert!(matches!(selected, Ok(Value::None)), "real selection failed: {selected:?}");
                assert_eq!(client.session.int("corpid"), Some(corp));
                assert_eq!(client.session.int("allianceid"), None);
                assert_eq!(client.session.int("hqID"), Some(station_id));
                for key in ["corprole", "rolesAtAll", "rolesAtHQ", "rolesAtBase", "rolesAtOther"] { assert_eq!(client.session.int(key), Some(0)); }
                assert_eq!(world.online_session(target.character).unwrap().corporation_id, Some(corp));
            }).await;
            if let Err(error) = before {
                let cleanup = client.leave_world().await;
                drop(peer);
                cleanup.expect("selected client cleanup failed");
                std::panic::resume_unwind(error);
            }
            let Some(mut registration) = crate::live_sessions::register(target.account, false) else {
                let cleanup = client.leave_world().await;
                drop(peer);
                cleanup.expect("selected client cleanup failed");
                panic!("unique live registration unavailable");
            };
            registration.wait_for_predecessor().await;
            let mut running = SelectionTask(Some(tokio::spawn(async move { client.run(&mut registration).await })), scenario_reapers.clone());
            let mut peer = peer;
            let checked = catch_race(async {
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    while crate::live_sessions::sender(target.account).is_none() { tokio::task::yield_now().await; }
                }).await.expect("real client run did not attach its sender");
                real_kick(&mut manager, corp, target.character).await;
                let new_corp = assert_departure(&other_db, target.character, corp, marker).await;
                let new_hq = sde.npc_corporations.get(new_corp).and_then(|row| row.station_id);
                read_corporation_change(&mut peer, corp, new_corp, station_id, new_hq).await;
                let session = world.online_session(target.character).unwrap();
                assert_eq!((session.corporation_id, session.alliance_id, session.corporation_role), (Some(new_corp), None, 0));
            }).await;
            let before = crate::db::outbox::tests::events(&other_db, "CharacterLoggedOff", target.character).await.len();
            use tokio::io::AsyncWriteExt;
            let shutdown = peer.shutdown().await;
            let cleanup = running.finish().await;
            drop(peer);
            shutdown.expect("peer write shutdown failed");
            cleanup.expect("real client run cleanup failed");
            let logoffs = crate::db::outbox::tests::events(&other_db, "CharacterLoggedOff", target.character).await;
            assert_eq!(logoffs.len(), before + 1);
            assert!(matches!(logoffs.last(), Some(crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id }) if (*user_id, *char_id) == (target.account, target.character)));
            assert!(world.online_session(target.character).is_none());
            if let Err(error) = checked { std::panic::resume_unwind(error); }
        }
        drop(manager_peer);
    }).await;
    reapers.join().await;
    // A failed scenario drops its gates and aborts its selector/run tasks. Wait for
    // their pool leases to return before removing only this test's fixtures.
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while selector_db.pool.status().available != selector_db.pool.status().size || selector_db.pool.status().waiting != 0 { tokio::task::yield_now().await; }
    }).await.expect("selector lease survived scenario cleanup");
    let ids = [ceo.character, target.character];
    kick_db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&crate::services::corps::exit::job_id(target.character)]).await.unwrap();
    kick_db.execute("DELETE FROM game.corp_pending_kicks WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    kick_db.execute("DELETE FROM game.corp_member_events WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    kick_db.execute("DELETE FROM game.corp_role_history WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    kick_db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap();
    kick_db.execute("DELETE FROM game.employment_history WHERE char_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    kick_db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=ANY($1))", &[&ids.as_slice()]).await.unwrap();
    kick_db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,ship_id=NULL WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    kick_db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&kick_db, ceo).await.unwrap();
    fixture::cleanup(&kick_db, target).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn real_kick_commits_before_published_selection_reread() {
    login_kick_order(true).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn real_selection_reread_precedes_kick_and_client_run_delivers_session() {
    login_kick_order(false).await;
}

impl Db {
    pub(crate) fn unconnected_publication_fixture(database_trap: std::net::SocketAddr) -> Self {
        let mut config = tokio_postgres::Config::new();
        config.host(&database_trap.ip().to_string()).port(database_trap.port());
        config.user("test").dbname("test");
        let manager = Manager::from_config(config, NoTls, ManagerConfig { recycling_method: RecyclingMethod::Verified });
        let pool = Pool::builder(manager).max_size(1).runtime(deadpool_postgres::Runtime::Tokio1).build().unwrap();
        Self { pool, online: Arc::new(AtomicI64::new(0)), progress_registry: Arc::new(crate::progress::Registry::default()) }
    }

    pub(crate) fn assert_publication_fixture_unused(&self) {
        let status = self.pool.status();
        assert_eq!(status.size, 0);
        assert_eq!(status.waiting, 0);
    }
}
