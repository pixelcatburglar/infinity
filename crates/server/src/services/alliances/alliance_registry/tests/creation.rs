//! Alliance creation dispatch, fee, locking and rollback regressions.
use super::*;
use infinity_core::consts::ACCOUNT_CASH;

fn args(corp: i64, prefix: char) -> Vec<Value> {
    vec![Value::unicode(format!("Alliance {prefix}{corp}")), Value::unicode(format!("{prefix}{:04X}", corp % 65536)), Value::unicode("description"), Value::unicode("url")]
}
async fn rpc(client: &mut Client, bind_corp: i64, values: &[Value]) -> CallResult {
    let bind = Value::Int(bind_corp);
    let kwargs = Value::None;
    let call = Call { service: "corpRegistry", method: "CreateAlliance", args: values, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    crate::services::corps::dispatch(client, &call).await.expect("creation RPC routed")
}

async fn wallet(s: &Scenario, corp: i64, amount: i64) -> Result<()> {
    s.f.db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,$3::bigint::numeric)", &[&corp, &ACCOUNT_CASH, &amount]).await?;
    Ok(())
}

async fn balance(s: &Scenario, corp: i64) -> Result<f64> {
    Ok(s.f.db.query_one("SELECT balance::float8 AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?.get("balance"))
}

async fn created(s: &Scenario, corp: i64) -> Result<i64> {
    Ok(s.f.db.query_one("SELECT count(*) AS n FROM game.alliances WHERE creator_corp_id=$1", &[&corp]).await?.get("n"))
}

async fn cleanup(s: Scenario) {
    s.f.db.transaction(|tx, _: &mut ()| {
        let corps = s.corps.clone();
        Box::pin(async move {
            crate::services::tests::fixture::cleanup_progress(tx, &corps, &[]).await
        })
    }, |(), ()| async move { Ok(()) }).await.unwrap();
    s.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn creation_dispatch_charges_exact_fee_and_checks_binding() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, _) = setup(&mut s).await?;
        s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
        wallet(&s, corp, 0).await?;
        s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        ensure!(rpc(&mut s.f.client, executor, &args(corp, 'A')).await.is_err(), "forged binding founded alliance");
        ensure!(rpc(&mut s.f.client, corp, &args(corp, 'A')[..3]).await.is_err(), "three arguments founded alliance");
        ensure!(rpc(&mut s.f.client, corp, &args(corp, 'A')).await.is_err(), "unfunded corporation founded alliance");
        ensure!(balance(&s, corp).await? == 0.0 && created(&s, corp).await? == 0 && drain(&mut s.joining).is_empty(), "refusal changed state");
        s.f.db.execute("UPDATE game.corp_wallets SET balance=2000000000 WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?;
        let result = rpc(&mut s.f.client, corp, &args(corp, 'A')).await.map_err(|e| anyhow!("creation: {e:?}"))?;
        let id = result.as_int().ok_or_else(|| anyhow!("creation did not return ID"))?;
        ensure!(balance(&s, corp).await? == 1_000_000_000.0 && state(&s, corp).await?.0 == Some(id) && state(&s, corp).await?.3 == 1 && created(&s, corp).await? == 1, "creation fee or membership incorrect");
        Ok(())
    }).await;
    cleanup(s).await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn concurrent_same_corporation_creations_charge_once() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, _, _) = setup(&mut s).await?;
        s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
        wallet(&s, corp, 2_000_000_000).await?;
        let (mut a, peer_a) = crate::services::tests::client(s.f.db.clone(), s.f.owner).await;
        let (mut b, peer_b) = crate::services::tests::client(s.f.db.clone(), s.f.owner).await;
        a.session.apply(vec![("corpid", Value::Int(corp))]); b.session.apply(vec![("corpid", Value::Int(corp))]);
        let first = args(corp, 'A'); let second = args(corp, 'B');
        let (x, y) = tokio::join!(rpc(&mut a, corp, &first), rpc(&mut b, corp, &second));
        drop((peer_a, peer_b));
        ensure!(x.is_ok() != y.is_ok(), "expected one creation winner: first={x:?}, second={y:?}");
        ensure!(created(&s, corp).await? == 1 && balance(&s, corp).await? == 1_000_000_000.0 && state(&s, corp).await?.3 == 1, "creation race charged twice or created extra history");
        Ok(())
    }).await;
    cleanup(s).await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn creation_rechecks_ceo_and_alliance_after_lock_wait() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
        wallet(&s, corp, 2_000_000_000).await?;
        for revoke_ceo in [true, false] {
            let (mut client, peer) = crate::services::tests::client(s.f.db.clone(), s.f.owner).await;
            client.session.apply(vec![("corpid", Value::Int(corp))]);
            let (lock, connection, pid) = barrier().await?;
            if revoke_ceo {
                tokio::time::timeout(TIMEOUT, lock.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&corp, &s.member])).await??;
            } else {
                tokio::time::timeout(TIMEOUT, lock.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&corp, &alliance])).await??;
            }
            let mut task = AbortTask(tokio::spawn(async move { let _peer = peer; rpc(&mut client, corp, &args(corp, 'C')).await.is_err() }));
            let wait = blocked(&s.f.db, pid, "SELECT corporation_id, alliance_id FROM game.corporations%FOR UPDATE", 1).await;
            let commit = tokio::time::timeout(TIMEOUT, lock.batch_execute("COMMIT")).await;
            drop(lock); drop(connection);
            let outcome = tokio::time::timeout(TIMEOUT, &mut task.0).await;
            if outcome.is_err() { task.0.abort(); let _ = (&mut task.0).await; return Err(anyhow!("creation timed out")); }
            commit??;
            wait?;
            ensure!(outcome??, "stale creation succeeded");
            ensure!(created(&s, corp).await? == 0 && balance(&s, corp).await? == 2_000_000_000.0 && state(&s, corp).await?.3 == 0, "stale creation had effects");
            s.f.db.execute("UPDATE game.corporations SET ceo_id=$2, alliance_id=NULL WHERE corporation_id=$1", &[&corp, &s.f.owner]).await?;
        }
        let _ = executor;
        Ok(())
    }).await;
    cleanup(s).await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn creation_history_failure_rolls_back_fee_and_new_alliance() -> Result<()> {
    let mut s = Scenario::new().await?;
    let (corp, _, _) = setup(&mut s).await?;
    s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
    wallet(&s, corp, 2_000_000_000).await?;
    s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
    let function = format!("alliance_create_fail_{corp}"); let trigger = function.clone();
    s.f.db.execute(&format!("CREATE FUNCTION game.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected creation failure'; END $$"), &[]).await?;
    if let Err(error) = s.f.db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.alliance_history FOR EACH ROW WHEN (NEW.corporation_id = {corp}) EXECUTE FUNCTION game.{function}()"), &[]).await {
        s.f.db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await?; cleanup(s).await; return Err(error.into());
    }
    let result = catch_scenario(async {
        let before = state(&s, corp).await?;
        ensure!(rpc(&mut s.f.client, corp, &args(corp, 'D')).await.is_err(), "injected failure accepted");
        ensure!(balance(&s, corp).await? == 2_000_000_000.0 && state(&s, corp).await? == before && created(&s, corp).await? == 0, "creation failure changed fee or rows");
        ensure!(drain(&mut s.joining).is_empty() && drain(&mut s.existing).is_empty(), "creation failure emitted effects");
        Ok(())
    }).await;
    let trigger_result = s.f.db.execute(&format!("DROP TRIGGER {trigger} ON game.alliance_history"), &[]).await;
    let function_result = s.f.db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await;
    cleanup(s).await;
    trigger_result?; function_result?;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
