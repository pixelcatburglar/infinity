//! Real pending-kick association drift through the scheduler's failure outcome.
use super::*;
use anyhow::{anyhow, ensure, Result};
use infinity_core::{consts::{FILETIME_SECOND, ROLE_DIRECTOR}};
use crate::{services::{self, tests::{self, fixture}, Call}, scheduler};
use marshal::Value;
use tokio::task::JoinSet;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn pending_association_drift_is_retried_by_real_runner_finish() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let marker: i64 = db.query_one("SELECT COALESCE(MAX(notification_id),0) AS marker FROM game.notifications WHERE receiver_id=ANY($1)", &[&vec![ceo.character,target.character]]).await.unwrap().get("marker");
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('drift-corp-' || nextval('game.corporations_corporation_id_seq')::text, 'DR' || currval('game.corporations_corporation_id_seq')::text, $1,$1,60000001,1) RETURNING corporation_id", &[&ceo.character]).await.unwrap().get("corporation_id");
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('drift-alliance-' || nextval('game.alliances_alliance_id_seq')::text, 'DA' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp, &ceo.character]).await.unwrap().get("alliance_id");
    let job_id = format!("corporations.pending_kick:{}", target.character);
    let test_db = db.clone();
    let task_job = job_id.clone();
    let scenario = tokio::spawn(async move {
        let mut handler_cfg = cfg.clone();
        handler_cfg.pool_size = 1;
        handler_cfg.schema_on_start = false;
        let handler_db = Db::connect(&handler_cfg).await?;
        let handler_pid: i32 = handler_db.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
        let ids = [ceo.character, target.character];
        test_db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await?;
        test_db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await?;
        let now = filetime_now();
        test_db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&target.character, &now]).await?;
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        client.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
        let args = [Value::Int(target.character)];
        let kwargs = Value::Dict(Vec::new());
        let binding = Value::Int(corp);
        let call = Call { service: "corpRegistry", method: "KickOutMember", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
        services::dispatch(&mut client, &call).await.map_err(|e| anyhow!("kick: {e:?}"))?;
        let pending = test_db.query_one("SELECT issuer_id,requested_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await?;
        let stamp: i64 = pending.get("requested_at");
        ensure!(pending.get::<_, i64>("issuer_id") == ceo.character, "wrong issuer");
        let past = now - (crate::config::game().corporations.role_removal_cooldown_secs + 1) * FILETIME_SECOND;
        test_db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&target.character, &past]).await?;
        test_db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&task_job, &now]).await?;
        let others: Vec<String> = test_db.query("SELECT job_id FROM game.jobs WHERE job_id<>$1", &[&task_job]).await?.into_iter().map(|row| row.get("job_id")).collect();
        let excluded: Vec<&str> = others.iter().map(String::as_str).collect();
        let claim = jobs::claim(&test_db, now, now + 60 * FILETIME_SECOND, 1, &excluded).await?.into_iter().find(|c| c.job.id == task_job).ok_or_else(|| anyhow!("fixture job not claimed"))?;
        let reg = scheduler::register(crate::services::corps::pending_kicks::KIND, crate::services::corps::pending_kicks::handle);
        let handlers = registry(vec![reg])?;
        let handler = handlers.get(crate::services::corps::pending_kicks::KIND).ok_or_else(|| anyhow!("registration missing"))?.handler;
        let pg = cfg.to_pg_config()?;
        let (mut gate, gate_connection) = pg.connect(tokio_postgres::NoTls).await?;
        let gate_driver = tokio::spawn(gate_connection);
        let gate_pid: i32 = gate.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
        let mut gate_tx = Some(gate.transaction().await?);
        let mut tasks = JoinSet::new();
        let run: Result<()> = async {
            let tx = gate_tx.as_ref().unwrap();
            tx.query_one("SELECT alliance_id FROM game.alliances WHERE alliance_id=$1 FOR UPDATE", &[&alliance]).await?;
            tx.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&corp, &alliance]).await?;
            let world = client.world.clone();
            let handle = tasks.spawn(handler(handler_db.clone(), world, claim.job));
            let id = handle.id();
            let mut active = HashMap::new();
            active.insert(id, ActiveClaim { id: task_job.clone(), token: claim.token, attempt: claim.attempt, interval_secs: None });
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let blocked: bool = test_db.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND $2=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock') AS blocked", &[&handler_pid, &gate_pid]).await?.get("blocked");
                    if blocked { break; }
                    tokio::task::yield_now().await;
                }
                Ok::<(), anyhow::Error>(())
            }).await.map_err(|_| anyhow!("handler did not block on association row"))??;
            gate_tx.take().unwrap().commit().await?;
            let outcome = tokio::time::timeout(Duration::from_secs(30), tasks.join_next_with_id()).await.map_err(|_| anyhow!("handler did not finish"))?.ok_or_else(|| anyhow!("handler task missing"))?;
            ensure!(outcome.as_ref().is_ok_and(|(_, result)| result.as_ref().is_err_and(|error| error.to_string().contains("association changed"))), "handler did not report association drift");
            finish(&test_db, &SchedulerConfig::default(), &mut active, outcome).await;
            ensure!(active.is_empty(), "active claim was not finished");
            let job = test_db.query_one("SELECT state,due_at,lease_token,last_error FROM game.jobs WHERE job_id=$1", &[&task_job]).await?;
            ensure!(job.get::<_, String>("state") == "scheduled", "job was not rescheduled");
            ensure!(job.get::<_, i64>("due_at") > filetime_now(), "retry deadline is not in the future");
            ensure!(job.get::<_, Option<i64>>("lease_token").is_none(), "lease was not cleared");
            ensure!(job.get::<_, Option<String>>("last_error").is_some_and(|text| text.contains("association changed")), "drift error was not recorded");
            let row = test_db.query_one("SELECT issuer_id,requested_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await?;
            ensure!((row.get::<_, i64>("issuer_id"), row.get::<_, i64>("requested_at")) == (ceo.character, stamp), "pending identity changed");
            ensure!(test_db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target.character]).await?.get::<_, i64>("corporation_id") == corp, "target departed");
            ensure!(test_db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target.character, &marker]).await?.is_empty(), "kick notice emitted");
            Ok(())
        }.await;
        if let Some(tx) = gate_tx.take() { let _ = tx.rollback().await; }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        drop(gate_tx);
        drop(gate);
        let _ = gate_driver.await;
        run
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&vec![ceo.character,target.character], &marker]).await.unwrap();
    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=ANY($1)", &[&vec![ceo.character,target.character]]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, target).await.unwrap();
    scenario.unwrap().unwrap();
}
