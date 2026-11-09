//! Rollback after a database failure following the membership write.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn history_failure_rolls_back_membership_and_outbox() -> Result<()> {
    let mut s = Scenario::new().await?;
    let (corp, _, alliance) = setup(&mut s).await?;
    s.app(alliance, corp).await?;
    s.f.client.session.apply(vec![("charid", Value::Int(s.f.owner)), ("corpid", Value::Int(corp))]);
    let function = format!("alliance_admission_fail_{corp}");
    let trigger = format!("alliance_admission_fail_{corp}");
    s.f.db.execute(&format!("CREATE FUNCTION game.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected history failure'; END $$"), &[]).await?;
    let created = s.f.db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.alliance_history FOR EACH ROW WHEN (NEW.corporation_id = {corp}) EXECUTE FUNCTION game.{function}()"), &[]).await;
    if let Err(error) = created {
        s.f.db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await?;
        s.cleanup().await;
        return Err(error.into());
    }
    let result = catch_scenario(async {
        let before = state(&s, corp).await?;
        let identity = crate::services::identity::Identity::of_client(&s.f.client);
        ensure!(admission::decide(&s.f.db, &s.f.world, &identity, alliance, corp, admission::Decision::Force { role: infinity_core::consts::GM_ROLES }).await.is_err(), "injected failure did not refuse admission");
        ensure!(state(&s, corp).await? == before && application(&s, alliance, corp).await? == Some(APP_NEW), "failed admission changed persistent rows");
        ensure!(drain(&mut s.joining).is_empty() && drain(&mut s.existing).is_empty(), "failed admission emitted effects");
        Ok(())
    }).await;
    s.f.db.execute(&format!("DROP TRIGGER {trigger} ON game.alliance_history"), &[]).await?;
    s.f.db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await?;
    s.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn competing_application_delete_failure_discards_queued_outbox() -> Result<()> {
    let mut s = Scenario::new().await?;
    let (corp, executor, alliance) = setup(&mut s).await?;
    let other = s.alliance(executor).await?;
    s.app(alliance, corp).await?; s.app(other, corp).await?;
    s.f.client.session.apply(vec![("charid", Value::Int(s.member)), ("corpid", Value::Int(executor))]);
    let function = format!("alliance_delete_fail_{corp}");
    let trigger = format!("alliance_delete_fail_{corp}");
    s.f.db.execute(&format!("CREATE FUNCTION game.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected application delete failure'; END $$"), &[]).await?;
    let created = s.f.db.execute(&format!("CREATE TRIGGER {trigger} BEFORE DELETE ON game.alliance_applications FOR EACH ROW WHEN (OLD.corporation_id = {corp} AND OLD.alliance_id = {other}) EXECUTE FUNCTION game.{function}()"), &[]).await;
    if let Err(error) = created {
        s.f.db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await?;
        s.cleanup().await;
        return Err(error.into());
    }
    let result = catch_scenario(async {
        let before = state(&s, corp).await?;
        let rows: String = s.f.db.query_one("SELECT string_agg(row_to_json(a)::text, ',' ORDER BY alliance_id) AS rows FROM game.alliance_applications a WHERE corporation_id=$1", &[&corp]).await?.get("rows");
        ensure!(!dispatch_update(&mut s, alliance, corp, APP_ACCEPTED, "UpdateApplication").await, "injected failure did not refuse admission");
        let after: String = s.f.db.query_one("SELECT string_agg(row_to_json(a)::text, ',' ORDER BY alliance_id) AS rows FROM game.alliance_applications a WHERE corporation_id=$1", &[&corp]).await?.get("rows");
        ensure!(state(&s, corp).await? == before && rows == after, "late failure changed membership, history or applications");
        ensure!(drain(&mut s.joining).is_empty() && drain(&mut s.existing).is_empty(), "queued effects escaped rollback");
        Ok(())
    }).await;
    let dropped_trigger = s.f.db.execute(&format!("DROP TRIGGER {trigger} ON game.alliance_applications"), &[]).await;
    let dropped_function = s.f.db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await;
    s.cleanup().await;
    dropped_trigger?; dropped_function?;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
