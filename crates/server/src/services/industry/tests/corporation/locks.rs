//! Industrial use of corporation blueprints subject to vote reservations and locks.
use super::*;
use infinity_core::{consts::{BLUEPRINT_INFINITE_RUNS, SINGLETON_QUANTITY}, messages::{self, corporations as text}};

async fn vote(client: &mut Client, method: &str, args: &[Value]) -> crate::services::CallResult {
    crate::services::dispatch(client, &Call { service: "voteManager", method, args, kwargs: &Value::None, bound: false, bind_params: None }).await
}

fn refused<T>(result: Result<T, CallError>, expected: &str) {
    match result {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(expected))]);
        }
        _ => panic!("expected CustomNotify"),
    }
}

async fn ready(db: &Db, client: &Client, id: i64) {
    db.execute("UPDATE game.industry_jobs SET end_date=0 WHERE job_id=$1", &[&id]).await.unwrap();
    completion::handle(db.clone(), client.world.clone(), crate::scheduler::Job { id: format!("industry.ready:{id}"), kind: completion::KIND.into(), due_at: 0, payload: id.to_string() }).await.unwrap();
    assert_eq!(industry_dao::job(db, id).await.unwrap().unwrap().status, INDUSTRY_JOB_STATUS_READY);
}

async fn custody(db: &Db, id: i64) -> String {
    db.query_one("SELECT row_to_json(i)::text AS state FROM game.items i WHERE item_id=$1", &[&id]).await.unwrap().get("state")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn locked_original_copy_and_research_preserve_custody_and_busy_reservations() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let (f, facility, _prices, _roles) = fixture(&db, &sde).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        assert!(crate::dao::corporations::shares::initialize(&db, f.corp, 100).await.unwrap());
        assert!(crate::dao::corporations::shares::transfer(&db, f.corp, f.corp, f.ceo, 100).await.unwrap());
        let (mut industrial, _industrial_peer) = crate::services::tests::client_with_sde(db.clone(), f.actor, sde.clone()).await;
        let (mut ceo, _ceo_peer) = crate::services::tests::client_with_sde(db.clone(), f.ceo, sde.clone()).await;
        for client in [&mut industrial, &mut ceo] {
            client.session.apply(vec![("corpid", Value::Int(f.corp)), ("stationid", Value::Int(f.station)), ("solarsystemid2", Value::Int(f.system))]);
        }
        let locations = authorization::locations(&db, &sde, f.actor, f.corp, &facility).await.ok().unwrap();
        let input = locations.iter().find(|l| l.flag_id == FLAG_CORP_SAG1).unwrap();
        let original = item(&db, f.bp, f.corp, f.office, input.flag_id, SINGLETON_QUANTITY, true).await;
        industry_dao::set_blueprint(&db, original, 0, 0, BLUEPRINT_INFINITE_RUNS).await.unwrap();
        let before = custody(&db, original).await;
        let descriptor = |label| Value::tuple([Value::str(label), Value::dict([("item", Value::Int(i64::from(f.bp)))])]);
        let proposal = [Value::unicode("Blueprint"), Value::unicode(""), Value::Int(5), Value::tuple([
            descriptor("UI/Corporations/CorporationWindow/Politics/LockdownItem"),
            Value::tuple([Value::str("UI/Corporations/CorporationWindow/Politics/DontLockdownItem")]),
            Value::Int(original), Value::Int(i64::from(f.bp)), Value::Int(f.station),
        ]), Value::Int(1)];
        let act = sde.industry.activity(f.bp, COPYING).unwrap();
        readiness::prepare_activity(&db, f.actor, act, input).await;
        let copy_request = request(f, original, COPYING, input, input, f.corp, ACCOUNT_CASH + 1);
        let id = mechanics::install(&mut industrial, &copy_request).await.ok().unwrap().as_int().unwrap();
        for make_ready in [false, true] {
            if make_ready { ready(&db, &industrial, id).await; }
            refused(vote(&mut ceo, "InsertVoteCase", &proposal).await, text::ITEM_LOCK_TARGET_INVALID);
            let count: i64 = db.query_one("SELECT count(*) AS n FROM game.corp_vote_cases WHERE corporation_id=$1", &[&f.corp]).await.unwrap().get("n");
            assert_eq!(count, 0);
        }
        mechanics::complete(&mut industrial, id).await.ok().unwrap();
        let case = vote(&mut ceo, "InsertVoteCase", &proposal).await.unwrap().as_int().unwrap();
        assert_eq!(crate::dao::corporations::votes::case(&db, case).await.unwrap().unwrap().reserved_item_id, Some(original));
        readiness::prepare_activity(&db, f.actor, act, input).await;
        let state: String = db.query_one("SELECT json_build_array((SELECT row_to_json(w) FROM game.corp_wallets w WHERE corporation_id=$1 AND account_key=$2), (SELECT json_agg(row_to_json(i) ORDER BY item_id) FROM game.items i WHERE owner_id=$1 AND location_id=$3 AND flag_id=$4 AND NOT singleton), (SELECT json_agg(row_to_json(j) ORDER BY job_id) FROM game.industry_jobs j WHERE blueprint_id=$5))::text AS state", &[&f.corp, &(ACCOUNT_CASH + 1), &input.item_id, &input.flag_id, &original]).await.unwrap().get("state");
        refused(mechanics::install(&mut industrial, &copy_request).await, text::ITEM_LOCKED_OR_RESERVED);
        let after: String = db.query_one("SELECT json_build_array((SELECT row_to_json(w) FROM game.corp_wallets w WHERE corporation_id=$1 AND account_key=$2), (SELECT json_agg(row_to_json(i) ORDER BY item_id) FROM game.items i WHERE owner_id=$1 AND location_id=$3 AND flag_id=$4 AND NOT singleton), (SELECT json_agg(row_to_json(j) ORDER BY job_id) FROM game.industry_jobs j WHERE blueprint_id=$5))::text AS state", &[&f.corp, &(ACCOUNT_CASH + 1), &input.item_id, &input.flag_id, &original]).await.unwrap().get("state");
        assert_eq!(state, after);
        assert_eq!(custody(&db, original).await, before);
        vote(&mut ceo, "InsertVote", &[Value::Int(f.corp), Value::Int(case), Value::Int(0)]).await.unwrap();
        let ends = crate::client::filetime_now() - 1;
        db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&case, &ends]).await.unwrap();
        let ends = crate::dao::corporations::votes::case(&db, case).await.unwrap().unwrap().ends;
        crate::services::corps::vote_close::handle(db.clone(), ceo.world.clone(), crate::scheduler::Job { id: format!("corp.vote.close:{case}"), kind: "corp.vote.close".into(), due_at: ends, payload: case.to_string() }).await.unwrap();
        vote(&mut ceo, "ActivateSanctionedAction", &[Value::Int(case)]).await.unwrap();
        assert!(crate::dao::items::corporation_locks::get(&db, original).await.unwrap().is_some());
        assert_eq!(crate::dao::corporations::votes::case(&db, case).await.unwrap().unwrap().reserved_item_id, None);
        let prior_copy: i64 = db.query_one("SELECT max(item_id) AS id FROM game.items WHERE owner_id=$1 AND type_id=$2 AND quantity=$3", &[&f.corp, &f.bp, &BLUEPRINT_COPY_QUANTITY]).await.unwrap().get("id");
        for activity in [COPYING, RESEARCH_MATERIAL, RESEARCH_TIME] {
            readiness::prepare_activity(&db, f.actor, sde.industry.activity(f.bp, activity).unwrap(), input).await;
            let id = mechanics::install(&mut industrial, &request(f, original, activity, input, input, f.corp, ACCOUNT_CASH + 1)).await.ok().unwrap().as_int().unwrap();
            assert_eq!(custody(&db, original).await, before); assert_eq!(industry_dao::blueprint(&db, original).await.unwrap().unwrap().runs, BLUEPRINT_INFINITE_RUNS);
            ready(&db, &industrial, id).await;
            mechanics::complete(&mut industrial, id).await.ok().unwrap();
            assert_eq!(industry_dao::job(&db, id).await.unwrap().unwrap().status, STATUS_DELIVERED);
            assert_eq!(custody(&db, original).await, before);
            assert!(crate::dao::items::corporation_locks::get(&db, original).await.unwrap().is_some());
            let bp = industry_dao::blueprint(&db, original).await.unwrap().unwrap();
            assert_eq!((bp.me, bp.te, bp.runs), (i32::from(activity != COPYING), if activity == RESEARCH_TIME { 2 } else { 0 }, BLUEPRINT_INFINITE_RUNS));
            if activity == COPYING {
                let copy: i64 = db.query_one("SELECT max(i.item_id) AS id FROM game.items i JOIN game.blueprints b USING(item_id) WHERE i.type_id=$1 AND i.owner_id=$2 AND i.location_id=$3 AND i.flag_id=$4 AND i.quantity=$5 AND i.singleton AND b.runs=1", &[&f.bp, &f.corp, &input.item_id, &input.flag_id, &BLUEPRINT_COPY_QUANTITY]).await.unwrap().get("id");
                assert!(copy > prior_copy && crate::dao::items::corporation_locks::get(&db, copy).await.unwrap().is_none());
            }
        }
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id IN (SELECT 'corp.vote.close:' || id::text FROM game.corp_vote_cases WHERE corporation_id=$1 UNION ALL SELECT 'corp.sanction.expire:' || id::text FROM game.corp_vote_cases WHERE corporation_id=$1)", &[&f.corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_item_locks WHERE corporation_id=$1", &[&f.corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_sanctioned_actions WHERE case_id IN (SELECT id FROM game.corp_vote_cases WHERE corporation_id=$1)", &[&f.corp]).await.unwrap();
    for table in ["corp_vote_claims", "corp_vote_ballots", "corp_vote_snapshot", "corp_vote_options"] {
        db.execute(&format!("DELETE FROM game.{table} WHERE case_id IN (SELECT id FROM game.corp_vote_cases WHERE corporation_id=$1)"), &[&f.corp]).await.unwrap();
    }
    db.execute("DELETE FROM game.corp_vote_cases WHERE corporation_id=$1", &[&f.corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&f.corp]).await.unwrap();
    cleanup(&db, f).await;
    result.unwrap();
}
