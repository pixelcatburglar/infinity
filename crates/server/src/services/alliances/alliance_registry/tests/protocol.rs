//! Bound alliance application protocol and indexed rowset regressions.
use super::*;

async fn corp_rpc(s: &mut Scenario, corp: i64, method: &str, args: &[Value]) -> CallResult {
    let bind = Value::Int(corp);
    let kwargs = Value::None;
    let call = Call { service: "corpRegistry", method, args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    crate::services::corps::dispatch(&mut s.f.client, &call).await.expect("corp RPC routed")
}

async fn alliance_rpc(s: &mut Scenario, alliance: i64, method: &str, args: &[Value]) -> CallResult {
    let bind = Value::tuple([Value::Int(alliance), Value::Bool(true)]);
    let kwargs = Value::None;
    let call = Call { service: "allianceRegistry", method, args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    dispatch(&mut s.f.client, &call).await.expect("alliance RPC routed")
}

fn indexed(value: Value, key: &str, expected: Option<(i64, i64, i64, &str, i32)>) -> Result<()> {
    let value = marshal::loads(&marshal::dumps(&value)).map_err(|e| anyhow!("wire decode: {e:?}"))?;
    let Value::Reduce { data, list, dict } = value else { return Err(anyhow!("not indexed rowset")); };
    ensure!(list.is_empty(), "unexpected indexed list");
    let parts = data.items().ok_or_else(|| anyhow!("missing reducer"))?;
    ensure!(parts[0] == Value::global("carbon.common.script.sys.crowset.CIndexedRowset"), "wrong rowset class");
    let args = parts[1].items().ok_or_else(|| anyhow!("missing rowset args"))?;
    ensure!(args.len() == 2 && args[1] == Value::str(key) && args[0] == application_header().to_value(), "wrong indexed header or key");
    if let Some((id, alliance, corp, text, state)) = expected {
        ensure!(dict.len() == 1 && dict[0].0 == Value::Int(id), "wrong index");
        let header = application_header();
        let row = &dict[0].1;
        ensure!(header.get(row, "allianceID") == Some(Value::Int(alliance)) && header.get(row, "corporationID") == Some(Value::Int(corp))
            && header.get(row, "applicationText") == Some(Value::unicode(text)) && header.get(row, "applicationDateTime").and_then(|v| v.as_int()).is_some()
            && header.get(row, "state") == Some(Value::Int(state as i64)), "wrong application columns");
    } else { ensure!(dict.is_empty(), "expected empty rowset"); }
    Ok(())
}

fn assert_application_diff(events: &[(String, Vec<Value>)], alliance: i64, corp: i64, old: Option<(&str, i64, i32)>, new: Option<(&str, i64, i32)>) -> Result<()> {
    let event = notice(events, "OnAllianceApplicationChanged", alliance).ok_or_else(|| anyhow!("missing application notice"))?;
    ensure!(event.get(1) == Some(&Value::Int(corp)), "wrong application recipient row");
    let side = |value: Option<(&str, i64, i32)>, column: usize| match (value, column) {
        (None, _) => Value::None,
        (Some(_), 0) => Value::Int(alliance),
        (Some(_), 1) => Value::Int(corp),
        (Some((text, _, _)), 2) => Value::unicode(text),
        (Some((_, created, _)), 3) => Value::Int(created),
        (Some((_, _, state)), _) => Value::Int(state as i64),
    };
    for (index, key) in ["allianceID", "corporationID", "applicationText", "applicationDateTime", "state"].iter().enumerate() {
        ensure!(pair(event, key, side(old, index), side(new, index)), "incorrect {key} diff: {event:?}");
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn application_rpc_roundtrip_and_rejected_withdrawal() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
        s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        indexed(corp_rpc(&mut s, corp, "GetAllianceApplications", &[]).await.map_err(|e| anyhow!("query: {e:?}"))?, "allianceID", None)?;
        ensure!(corp_rpc(&mut s, executor, "ApplyToJoinAlliance", &[Value::Int(alliance), Value::unicode("request")]).await.is_err(), "forged binding applied");
        ensure!(corp_rpc(&mut s, executor, "GetAllianceApplications", &[]).await.is_err(), "forged binding queried applications");
        ensure!(corp_rpc(&mut s, corp, "ApplyToJoinAlliance", &[Value::Int(alliance), Value::unicode("request")]).await.is_ok(), "application refused");
        let created: i64 = s.f.db.query_one("SELECT created FROM game.alliance_applications WHERE alliance_id=$1 AND corporation_id=$2", &[&alliance, &corp]).await?.get("created");
        for events in [drain(&mut s.joining), drain(&mut s.existing)] { assert_application_diff(&events, alliance, corp, None, Some(("request", created, APP_NEW)))?; }
        ensure!(corp_rpc(&mut s, corp, "ApplyToJoinAlliance", &[Value::Int(alliance), Value::unicode("again")]).await.is_err(), "duplicate application accepted");
        indexed(corp_rpc(&mut s, corp, "GetAllianceApplications", &[]).await.map_err(|e| anyhow!("query: {e:?}"))?, "allianceID", Some((alliance, alliance, corp, "request", APP_NEW)))?;
        s.f.client.session.apply(vec![("charid", Value::Int(s.member)), ("corpid", Value::Int(executor))]);
        s.pilot(s.member, executor, 0).await?;
        ensure!(alliance_rpc(&mut s, alliance, "GetApplications", &[]).await.is_err(), "revoked director read applications");
        s.pilot(s.member, executor, ROLE_DIRECTOR).await?;
        let wrong = s.alliance(executor).await?;
        ensure!(alliance_rpc(&mut s, wrong, "GetApplications", &[]).await.is_err(), "executor outside alliance read applications");
        indexed(alliance_rpc(&mut s, alliance, "GetApplications", &[]).await.map_err(|e| anyhow!("query: {e:?}"))?, "corporationID", Some((corp, alliance, corp, "request", APP_NEW)))?;
        ensure!(dispatch_update(&mut s, alliance, corp, APP_REJECTED, "UpdateApplication").await, "rejection refused");
        for events in [drain(&mut s.joining), drain(&mut s.existing)] { assert_application_diff(&events, alliance, corp, Some(("request", created, APP_NEW)), Some(("manager edit", created, APP_REJECTED)))?; }
        s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        indexed(corp_rpc(&mut s, corp, "GetAllianceApplications", &[]).await.map_err(|e| anyhow!("query: {e:?}"))?, "allianceID", Some((alliance, alliance, corp, "manager edit", APP_REJECTED)))?;
        ensure!(corp_rpc(&mut s, corp, "DeleteAllianceApplication", &[Value::Int(alliance)]).await.is_ok(), "rejected withdrawal refused");
        for events in [drain(&mut s.joining), drain(&mut s.existing)] { assert_application_diff(&events, alliance, corp, Some(("manager edit", created, APP_REJECTED)), None)?; }
        ensure!(corp_rpc(&mut s, corp, "ApplyToJoinAlliance", &[Value::Int(alliance), Value::unicode("again")]).await.is_ok(), "reapplication after withdrawal refused");
        let created: i64 = s.f.db.query_one("SELECT created FROM game.alliance_applications WHERE alliance_id=$1 AND corporation_id=$2", &[&alliance, &corp]).await?.get("created");
        for events in [drain(&mut s.joining), drain(&mut s.existing)] { assert_application_diff(&events, alliance, corp, None, Some(("again", created, APP_NEW)))?; }
        ensure!(corp_rpc(&mut s, corp, "DeleteAllianceApplication", &[Value::Int(alliance)]).await.is_ok(), "NEW withdrawal refused");
        for events in [drain(&mut s.joining), drain(&mut s.existing)] { assert_application_diff(&events, alliance, corp, Some(("again", created, APP_NEW)), None)?; }
        indexed(corp_rpc(&mut s, corp, "GetAllianceApplications", &[]).await.map_err(|e| anyhow!("query: {e:?}"))?, "allianceID", None)?;
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn offline_member_inherits_database_alliance() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
        let (offline, _) = s.f.add_pilot().await;
        s.pilot(offline, corp, 0).await?;
        s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        ensure!(corp_rpc(&mut s, corp, "ApplyToJoinAlliance", &[Value::Int(alliance), Value::unicode("request")]).await.is_ok(), "application refused");
        indexed(corp_rpc(&mut s, corp, "GetAllianceApplications", &[]).await.map_err(|e| anyhow!("query: {e:?}"))?, "allianceID", Some((alliance, alliance, corp, "request", APP_NEW)))?;
        drain(&mut s.joining); drain(&mut s.existing);
        s.f.client.session.apply(vec![("charid", Value::Int(s.member)), ("corpid", Value::Int(executor))]);
        indexed(alliance_rpc(&mut s, alliance, "GetApplications", &[]).await.map_err(|e| anyhow!("query: {e:?}"))?, "corporationID", Some((corp, alliance, corp, "request", APP_NEW)))?;
        ensure!(dispatch_update(&mut s, alliance, corp, APP_ACCEPTED, "UpdateApplication").await, "acceptance refused");
        let row = s.f.db.query_one("SELECT co.alliance_id FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id WHERE ch.character_id = $1", &[&offline]).await?;
        ensure!(row.get::<_, Option<i64>>("alliance_id") == Some(alliance), "offline member database identity is stale");
        let events = drain(&mut s.joining);
        ensure!(events.first().is_some_and(|e| e.0 == "__session") && notice(&events, "OnAllianceMemberChanged", alliance).is_some(), "online session not ordered before rows");
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn terminal_applications_cannot_be_overwritten() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, _, alliance) = setup(&mut s).await?;
        s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
        s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        s.app(alliance, corp).await?;
        for state in [APP_ACCEPTED, APP_EFFECTIVE, APP_REJECTED] {
            s.f.db.execute("UPDATE game.alliance_applications SET state=$3 WHERE alliance_id=$1 AND corporation_id=$2", &[&alliance, &corp, &state]).await?;
            let before: String = s.f.db.query_one("SELECT row_to_json(a)::text AS row FROM game.alliance_applications a WHERE alliance_id=$1 AND corporation_id=$2", &[&alliance, &corp]).await?.get("row");
            ensure!(matches!(corp_rpc(&mut s, corp, "ApplyToJoinAlliance", &[Value::Int(alliance), Value::unicode("overwrite")]).await,
                Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CANT_APPLY_TWICE_TO_ALLIANCE && dict.is_empty()), "terminal application did not return duplicate-application refusal");
            let after: String = s.f.db.query_one("SELECT row_to_json(a)::text AS row FROM game.alliance_applications a WHERE alliance_id=$1 AND corporation_id=$2", &[&alliance, &corp]).await?.get("row");
            ensure!(before == after && drain(&mut s.joining).is_empty() && drain(&mut s.existing).is_empty(), "terminal refusal changed row or sent notices");
        }
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn stale_bound_corporation_cannot_read_applications() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        s.app(alliance, corp).await?;
        let (lock, connection, pid) = barrier().await?;
        lock.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&s.f.owner, &executor]).await?;
        let (mut client, peer) = crate::services::tests::client(s.f.db.clone(), s.f.owner).await;
        client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        let mut task = tokio::spawn(async move {
            let _peer = peer;
            let bind = Value::Int(corp);
            let kwargs = Value::None;
            let call = Call { service: "corpRegistry", method: "GetAllianceApplications", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
            crate::services::corps::dispatch(&mut client, &call).await.expect("RPC routed").is_err()
        });
        let wait = blocked(&s.f.db, pid, "SELECT character_id FROM game.characters%FOR UPDATE", 1).await;
        lock.batch_execute("COMMIT").await?; drop(lock); connection.abort();
        let result = tokio::time::timeout(TIMEOUT, &mut task).await;
        if result.is_err() { task.abort(); let _ = task.await; return Err(anyhow!("query timed out")); }
        wait?;
        ensure!(result??, "stale corporation received private applications");
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn manager_can_reject_new_application_for_already_allied_corp() -> Result<()> {
    let mut s = Scenario::new().await?;
    let result = catch_scenario(async {
        let (corp, executor, alliance) = setup(&mut s).await?;
        s.pilot(s.f.owner, corp, ROLE_DIRECTOR).await?;
        s.app(alliance, corp).await?;
        s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
        ensure!(crate::services::corps::alliance_corp_registry::gm_join(&mut s.f.client, alliance).await.is_ok(), "GM join failed");
        drain(&mut s.joining); drain(&mut s.existing);
        s.f.client.session.apply(vec![("charid", Value::Int(s.member)), ("corpid", Value::Int(executor))]);
        ensure!(dispatch_update(&mut s, alliance, corp, APP_REJECTED, "UpdateApplication").await, "allied applicant rejection refused");
        ensure!(state(&s, corp).await?.0 == Some(alliance) && application(&s, alliance, corp).await? == Some(APP_REJECTED), "rejection changed alliance");
        Ok(())
    }).await;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
