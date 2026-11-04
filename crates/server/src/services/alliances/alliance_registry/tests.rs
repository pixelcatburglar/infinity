//! Database regressions for atomic alliance admission and post-commit notifications.
//! These use the real alliance and corporation registry paths and game tables.
//! A dedicated, already provisioned database with SDE data is required.
use super::*;
use anyhow::{anyhow, ensure, Result};
use std::{future::Future, task::Poll, time::Duration};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use crate::services::drones::tests::fixture::Fixture;

const TIMEOUT: Duration = Duration::from_secs(15);
type Notices = UnboundedReceiver<(String, Vec<Value>)>;

async fn catch_scenario(future: impl Future<Output = Result<()>>) -> std::thread::Result<Result<()>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
        Ok(Poll::Pending) => Poll::Pending,
        Err(panic) => Poll::Ready(Err(panic)),
    }).await
}

struct Scenario {
    f: Fixture,
    corps: Vec<i64>,
    alliances: Vec<i64>,
    member: i64,
    joining: Notices,
    existing: Notices,
}

impl Scenario {
    async fn new() -> Result<Self> {
        let mut f = Fixture::new().await;
        let (member, _) = f.add_pilot().await;
        let (tx, joining) = unbounded_channel();
        let (tx2, existing) = unbounded_channel();
        f.world.register_pilot(f.owner, tx);
        f.world.register_pilot(member, tx2);
        Ok(Self { f, corps: Vec::new(), alliances: Vec::new(), member, joining, existing })
    }

    async fn corp(&mut self, ceo: i64) -> Result<i64> {
        let row = self.f.db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('admission-corp-' || nextval('game.corporations_corporation_id_seq')::text, 'AC' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&ceo]).await?;
        let id = row.get("corporation_id");
        self.corps.push(id);
        Ok(id)
    }

    async fn alliance(&mut self, executor: i64) -> Result<i64> {
        let row = self.f.db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('admission-alliance-' || nextval('game.alliances_alliance_id_seq')::text, 'AA' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&executor, &self.f.owner]).await?;
        let id = row.get("alliance_id");
        self.alliances.push(id);
        Ok(id)
    }

    async fn app(&self, alliance: i64, corp: i64) -> Result<()> {
        self.f.db.execute("INSERT INTO game.alliance_applications (alliance_id,corporation_id,application_text,state,created) VALUES ($1,$2,'application',$3,1)", &[&alliance, &corp, &APP_NEW]).await?;
        Ok(())
    }

    async fn pilot(&mut self, pilot: i64, corp: i64, roles: i64) -> Result<()> {
        self.f.db.execute("UPDATE game.characters SET corporation_id=$2, corp_roles=$3 WHERE character_id=$1", &[&pilot, &corp, &roles]).await?;
        Ok(())
    }

    async fn cleanup(mut self) {
        self.f.db.execute("UPDATE game.characters SET corporation_id=1, corp_roles=0 WHERE user_id=$1", &[&self.f.account]).await.unwrap();
        self.f.client.session.apply(vec![("charid", Value::Int(self.f.owner)), ("corpid", Value::Int(1)), ("allianceid", Value::None)]);
        self.f.db.execute("DELETE FROM game.alliance_history WHERE corporation_id = ANY($1)", &[&self.corps]).await.unwrap();
        self.f.db.execute("DELETE FROM game.alliance_applications WHERE corporation_id = ANY($1) OR alliance_id = ANY($2)", &[&self.corps, &self.alliances]).await.unwrap();
        self.f.db.execute("DELETE FROM game.corporations WHERE corporation_id = ANY($1)", &[&self.corps]).await.unwrap();
        self.f.db.execute("DELETE FROM game.alliances WHERE alliance_id = ANY($1)", &[&self.alliances]).await.unwrap();
        self.f.cleanup().await;
    }
}

async fn dispatch_update(s: &mut Scenario, alliance: i64, corp: i64, state: i32, method: &str) -> bool {
    let args = [Value::Int(corp), Value::unicode("application"), Value::Int(state as i64)];
    let kwargs = Value::None;
    let bind = Value::tuple([Value::Int(alliance), Value::Bool(true)]);
    let call = Call { service: "allianceRegistry", method, args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    dispatch(&mut s.f.client, &call).await.is_some_and(|r| r.is_ok())
}

fn drain(rx: &mut Notices) -> Vec<(String, Vec<Value>)> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() { out.push(event); }
    out
}

fn field<'a>(args: &'a [Value], key: &str) -> Option<&'a Value> {
    let Value::Dict(fields) = args.last()? else { return None };
    fields.iter().find(|(k, _)| *k == Value::str(key)).map(|(_, v)| v)
}

fn pair(args: &[Value], key: &str, old: Value, new: Value) -> bool {
    field(args, key) == Some(&Value::tuple([old, new]))
}

fn notice<'a>(events: &'a [(String, Vec<Value>)], name: &str, alliance: i64) -> Option<&'a Vec<Value>> {
    events.iter().find(|(event, args)| event == name && args.first() == Some(&Value::Int(alliance))).map(|(_, args)| args)
}

fn assert_success_notices(events: &[(String, Vec<Value>)], alliance: i64, corp: i64, joining: bool) -> Result<()> {
    if joining {
        let (event, args) = events.first().ok_or_else(|| anyhow!("missing session"))?;
        ensure!(event == "__session" && args == &vec![Value::tuple([Value::str("allianceid"), Value::Int(alliance)])], "session must precede rows: {events:?}");
    }
    let member = notice(events, "OnAllianceMemberChanged", alliance).ok_or_else(|| anyhow!("missing member notice"))?;
    ensure!(member.get(1) == Some(&Value::Int(corp)) && pair(member, "corporationID", Value::None, Value::Int(corp)) && pair(member, "allianceID", Value::None, Value::Int(alliance)), "bad member notice: {member:?}");
    let row = notice(events, "OnAllianceChanged", alliance).ok_or_else(|| anyhow!("missing alliance notice"))?;
    ensure!(pair(row, "memberCount", Value::Int(1), Value::Int(2)) && pair(row, "corporationCount", Value::Int(1), Value::Int(2)), "bad alliance counts: {row:?}");
    Ok(())
}

async fn state(s: &Scenario, corp: i64) -> Result<(Option<i64>, Option<i64>, Option<i64>, i64)> {
    let row = s.f.db.query_one("SELECT alliance_id, alliance_joined, chosen_executor_id, (SELECT count(*) FROM game.alliance_history WHERE corporation_id=$1) AS histories FROM game.corporations WHERE corporation_id=$1", &[&corp]).await?;
    Ok((row.get("alliance_id"), row.get("alliance_joined"), row.get("chosen_executor_id"), row.get("histories")))
}

async fn application(s: &Scenario, alliance: i64, corp: i64) -> Result<Option<i32>> {
    Ok(s.f.db.query_opt("SELECT state FROM game.alliance_applications WHERE alliance_id=$1 AND corporation_id=$2", &[&alliance, &corp]).await?.map(|r| r.get("state")))
}

async fn setup(s: &mut Scenario) -> Result<(i64, i64, i64)> {
    let joining = s.corp(s.f.owner).await?;
    let executor = s.corp(s.member).await?;
    let alliance = s.alliance(executor).await?;
    s.f.db.execute("UPDATE game.corporations SET alliance_id=$2, alliance_joined=1 WHERE corporation_id=$1", &[&executor, &alliance]).await?;
    s.f.db.execute("UPDATE game.corporations SET chosen_executor_id=$2 WHERE corporation_id=$1", &[&joining, &executor]).await?;
    s.pilot(s.f.owner, joining, 0).await?;
    s.pilot(s.member, executor, ROLE_DIRECTOR).await?;
    Ok((joining, executor, alliance))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn acceptance_dispatch_and_refusals() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        let other = s.alliance(executor).await?;
        let third = s.alliance(executor).await?;
        for id in [alliance, other, third] { s.app(id, corp).await?; }
        s.f.client.session.apply(vec![("charid", Value::Int(s.member)), ("corpid", Value::Int(executor))]);
        s.pilot(s.member, executor, 0).await?;
        ensure!(!dispatch_update(&mut s, alliance, corp, APP_ACCEPTED, "UpdateApplication").await, "non-director accepted");
        ensure!(state(&s, corp).await?.3 == 0 && drain(&mut s.joining).is_empty() && drain(&mut s.existing).is_empty(), "unauthorized call had effects");
        s.pilot(s.member, executor, ROLE_DIRECTOR).await?;
        ensure!(dispatch_update(&mut s, alliance, corp, APP_ACCEPTED, "UpdateApplication").await, "authorized acceptance refused");
        let (joined, time, support, history) = state(&s, corp).await?;
        ensure!(joined == Some(alliance) && time.is_some_and(|v| v > 0) && support.is_none() && history == 1, "membership/history incorrect");
        ensure!(application(&s, alliance, corp).await? == Some(APP_EFFECTIVE) && application(&s, other, corp).await?.is_none() && application(&s, third, corp).await?.is_none(), "application changes incorrect");
        let a = drain(&mut s.joining); let b = drain(&mut s.existing);
        assert_success_notices(&a, alliance, corp, true)?;
        assert_success_notices(&b, alliance, corp, false)?;
        for events in [&a, &b] {
            let app = notice(events, "OnAllianceApplicationChanged", alliance).ok_or_else(|| anyhow!("missing acceptance notice"))?;
            ensure!(pair(app, "state", Value::Int(APP_NEW as i64), Value::Int(APP_EFFECTIVE as i64)), "bad state transition");
        }
        for id in [other, third] {
            let app = notice(&a, "OnAllianceApplicationChanged", id).ok_or_else(|| anyhow!("missing removal notice"))?;
            ensure!(pair(app, "state", Value::Int(APP_NEW as i64), Value::None), "bad removal notice");
        }
        ensure!(!dispatch_update(&mut s, alliance, corp, APP_ACCEPTED, "UpdateApplication").await, "repeat accepted");
        ensure!(admit(&s.f.db, &s.f.world, alliance, -1, true).await.is_err(), "missing corporation accepted");
        let fresh = s.corp(s.f.owner).await?;
        ensure!(admit(&s.f.db, &s.f.world, -1, fresh, false).await.is_err(), "missing alliance accepted");
        ensure!(admit(&s.f.db, &s.f.world, alliance, fresh, true).await.is_err(), "missing application accepted");
        ensure!(state(&s, corp).await?.3 == 1 && state(&s, fresh).await?.3 == 0 && drain(&mut s.joining).is_empty() && drain(&mut s.existing).is_empty(), "refusal had effects");
        s.app(alliance, fresh).await?;
        ensure!(dispatch_update(&mut s, alliance, fresh, APP_REJECTED, "UpdateApplication").await, "rejection refused");
        ensure!(application(&s, alliance, fresh).await? == Some(APP_REJECTED) && state(&s, fresh).await?.0.is_none() && state(&s, fresh).await?.3 == 0, "rejection changed membership");
        drain(&mut s.joining); drain(&mut s.existing);
        ensure!(dispatch_update(&mut s, alliance, fresh, APP_NEW, "GMForceAcceptApplication").await, "GM force acceptance refused");
        ensure!(application(&s, alliance, fresh).await? == Some(APP_EFFECTIVE) && state(&s, fresh).await?.0 == Some(alliance) && state(&s, fresh).await?.3 == 1, "GM force acceptance incorrect");
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn gm_join_preserves_target_application() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        let other = s.alliance(executor).await?;
        s.app(other, corp).await?;
        s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        ensure!(super::super::super::corps::alliance_corp_registry::gm_join(&mut s.f.client, alliance).await.is_ok(), "GM join refused");
        ensure!(application(&s, alliance, corp).await?.is_none() && application(&s, other, corp).await?.is_none() && state(&s, corp).await?.3 == 1, "GM application/history incorrect");
        assert_success_notices(&drain(&mut s.joining), alliance, corp, true)?;
        assert_success_notices(&drain(&mut s.existing), alliance, corp, false)?;
        let second = s.corp(s.f.owner).await?;
        s.pilot(s.f.owner, second, 0).await?;
        s.f.client.session.apply(vec![("corpid", Value::Int(second)), ("allianceid", Value::None)]);
        s.app(alliance, second).await?; s.app(other, second).await?;
        ensure!(super::super::super::corps::alliance_corp_registry::gm_join(&mut s.f.client, alliance).await.is_ok(), "GM join with application refused");
        ensure!(application(&s, alliance, second).await? == Some(APP_NEW) && application(&s, other, second).await?.is_none() && state(&s, second).await?.3 == 1, "GM changed target application");
        let events = drain(&mut s.joining);
        ensure!(events.first().is_some_and(|e| e.0 == "__session") && notice(&events, "OnAllianceMemberChanged", alliance).is_some(), "missing ordered GM notices");
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

async fn barrier() -> Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>, i32)> {
    let config = crate::config::tests::test_database().to_pg_config()?;
    let (client, connection) = config.connect(tokio_postgres::NoTls).await?;
    let task = tokio::spawn(async move { let _ = connection.await; });
    let pid = client.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
    client.batch_execute("BEGIN").await?;
    Ok((client, task, pid))
}

async fn blocked(db: &Db, pid: i32, statement: &str, count: i64) -> Result<()> {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let row = db.query_one("WITH RECURSIVE waiting(pid) AS (SELECT pid FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) UNION SELECT a.pid FROM pg_stat_activity a JOIN waiting w ON w.pid = ANY(pg_blocking_pids(a.pid))) SELECT count(*) AS n FROM pg_stat_activity WHERE pid IN (SELECT pid FROM waiting) AND query LIKE $2", &[&pid, &statement]).await?;
            if row.get::<_, i64>("n") >= count { return Ok::<(), anyhow::Error>(()); }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.map_err(|_| anyhow!("timed out waiting for blocked {statement}"))??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn concurrent_acceptances_have_one_winner() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, a) = setup(&mut s).await?;
        let b = s.alliance(executor).await?;
        s.app(a, corp).await?; s.app(b, corp).await?;
        let (lock, connection, pid) = barrier().await?;
        lock.query_one("SELECT corporation_id FROM game.corporations WHERE corporation_id=$1 FOR UPDATE", &[&corp]).await?;
        let db = s.f.db.clone(); let world = s.f.world.clone();
        let mut one = tokio::spawn(async move { admit(&db, &world, a, corp, true).await.is_ok() });
        let db = s.f.db.clone(); let world = s.f.world.clone();
        let mut two = tokio::spawn(async move { admit(&db, &world, b, corp, true).await.is_ok() });
        let wait = blocked(&s.f.db, pid, "UPDATE game.corporations SET alliance_id%", 2).await;
        lock.batch_execute("COMMIT").await?; drop(lock); connection.abort();
        let x = match tokio::time::timeout(TIMEOUT, &mut one).await {
            Ok(result) => result?,
            Err(_) => { one.abort(); two.abort(); let _ = tokio::join!(one, two); return Err(anyhow!("admission tasks timed out")); }
        };
        let y = match tokio::time::timeout(TIMEOUT, &mut two).await {
            Ok(result) => result?,
            Err(_) => { two.abort(); let _ = two.await; return Err(anyhow!("admission task timed out")); }
        };
        wait?;
        ensure!(x != y, "expected exactly one winner");
        let (winner, loser) = if x { (a, b) } else { (b, a) };
        ensure!(state(&s, corp).await?.0 == Some(winner) && state(&s, corp).await?.3 == 1, "winner overwritten or extra history");
        ensure!(application(&s, winner, corp).await? == Some(APP_EFFECTIVE) && application(&s, loser, corp).await?.is_none(), "winner cleanup incorrect");
        let events = drain(&mut s.joining);
        ensure!(events.iter().filter(|e| e.0 == "__session").count() == 1 && notice(&events, "OnAllianceMemberChanged", loser).is_none() && notice(&events, "OnAllianceMemberChanged", winner).is_some(), "loser emitted membership effects");
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn stale_application_rolls_back_late_admission() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        let other = s.alliance(executor).await?;
        s.app(alliance, corp).await?; s.app(other, corp).await?;
        let before = state(&s, corp).await?;
        let (lock, connection, pid) = barrier().await?;
        lock.execute("UPDATE game.alliance_applications SET state=$3 WHERE alliance_id=$1 AND corporation_id=$2", &[&alliance, &corp, &APP_REJECTED]).await?;
        let db = s.f.db.clone(); let world = s.f.world.clone();
        let mut task = tokio::spawn(async move { admit(&db, &world, alliance, corp, true).await.is_ok() });
        let wait = blocked(&s.f.db, pid, "UPDATE game.alliance_applications SET state%", 1).await;
        lock.batch_execute("COMMIT").await?; drop(lock); connection.abort();
        let joined = tokio::time::timeout(TIMEOUT, &mut task).await;
        if joined.is_err() { task.abort(); let _ = task.await; return Err(anyhow!("admission task timed out")); }
        let accepted = joined??;
        wait?;
        ensure!(!accepted && state(&s, corp).await? == before, "late failure did not roll back membership/history");
        ensure!(application(&s, alliance, corp).await? == Some(APP_REJECTED) && application(&s, other, corp).await? == Some(APP_NEW), "rollback lost application changes");
        ensure!(drain(&mut s.joining).is_empty() && drain(&mut s.existing).is_empty(), "rollback emitted notices");
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
