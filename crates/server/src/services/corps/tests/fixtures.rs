//! Transaction-local corporation fixtures and notification observation baselines.
use tokio_postgres::{Client, NoTls};

pub(super) async fn notification_marker(db: &crate::db::Db, receivers: &[i64]) -> i64 {
    db.query_one("SELECT COALESCE(MAX(notification_id),0) AS marker FROM game.notifications WHERE receiver_id=ANY($1)", &[&receivers]).await.unwrap().get("marker")
}

pub(super) async fn connect() -> Client {
    let cfg = crate::config::tests::test_database();
    let (client, connection) = cfg.to_pg_config().unwrap().connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}

pub(super) async fn character(tx: &(impl crate::db::executor::Executor + ?Sized)) -> i64 {
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('governance-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'governance-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id")
}

pub(super) async fn corporation(tx: &(impl crate::db::executor::Executor + ?Sized), ceo: i64) -> i64 {
    let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('governance-fixture-' || nextval('game.corporations_corporation_id_seq')::text,'GF' || currval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&ceo]).await.unwrap().get("corporation_id");
    tx.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$3 WHERE character_id=$2", &[&corp, &ceo, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();

    corp
}
pub(super) async fn original_blueprint(
    db: &crate::db::Db, sde: &crate::sde::Sde, corp: i64, office: i64,
) -> (i64, i32) {
    use crate::sde::industry::{COPYING, MANUFACTURING, RESEARCH_MATERIAL, RESEARCH_TIME};
    use infinity_core::consts::{BLUEPRINT_INFINITE_RUNS, CATEGORY_BLUEPRINT, FLAG_CORP_SAG1, SINGLETON_QUANTITY};
    let kind = sde.types.iter().find(|ty| {
        ty.category_id == Some(CATEGORY_BLUEPRINT)
            && [MANUFACTURING, COPYING, RESEARCH_MATERIAL, RESEARCH_TIME]
                .into_iter().all(|activity| sde.industry.activity(ty.type_id, activity).is_some())
    }).expect("SDE has no blueprint supporting manufacturing, copying and both research activities").type_id;
    let item = crate::dao::items::transfers::insert(db, kind, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true,quantity=$2 WHERE item_id=$1", &[&item, &SINGLETON_QUANTITY]).await.unwrap();
    crate::dao::industry::set_blueprint(db, item, 0, 0, BLUEPRINT_INFINITE_RUNS).await.unwrap();
    (item, kind)
}

pub(super) async fn general_vote_fixture(
    ex: &(impl crate::db::executor::Executor + ?Sized),
    corporation: i64, creator: i64, starts: i64, ends: i64,
) -> Result<i64, crate::db::Error> {
    use crate::dao::corporations::votes::{self, NewCase, NewOption};
    let case = votes::create(ex, &NewCase {
        corporation_id: corporation, creator_id: creator, title: "fixture", description: "",
        vote_type: super::super::vote_request::VOTE_GENERAL, starts, ends,
        previous_ceo_id: None, previous_ceo_was_active: None,
    }).await?;
    for (option_id, text) in [(0, "yes"), (1, "no")] {
        votes::add_option(ex, case, &NewOption {
            option_id, text, parameter: None, parameter1: None, parameter2: None,
        }).await?;
    }
    votes::snapshot(ex, case, corporation).await?;
    let kind = super::super::vote_close::KIND;
    crate::dao::jobs::schedule(ex, &crate::scheduler::Job {
        id: format!("{kind}:{case}"), kind: kind.into(), due_at: ends, payload: case.to_string(),
    }, starts).await?;
    Ok(case)
}

pub(super) async fn vote_call(
    client: &mut crate::client::Client, method: &str, args: &[marshal::Value],
) -> crate::services::CallResult {
    let kwargs = marshal::Value::Dict(vec![]);
    let call = crate::services::Call {
        service: "voteManager", method, args, kwargs: &kwargs, bound: false, bind_params: None,
    };
    crate::services::dispatch(client, &call).await
}

pub(super) fn typed_vote_options(labels: [&str; 2], parameters: [Option<i64>; 3]) -> marshal::Value {
    use marshal::Value;
    let descriptor = |label: &str| Value::tuple([Value::str(label)]);
    let number = |value: Option<i64>| value.map(Value::Int).unwrap_or(Value::None);
    Value::tuple([
        descriptor(labels[0]), descriptor(labels[1]),
        number(parameters[0]), number(parameters[1]), number(parameters[2]),
    ])
}

pub(super) async fn cleanup_votes(
    ex: &(impl crate::db::executor::Executor + ?Sized), corporations: &[i64],
) -> Result<(), crate::db::Error> {
    ex.execute("DELETE FROM game.jobs WHERE job_id IN (SELECT 'corp.vote.close:' || id::text FROM game.corp_vote_cases WHERE corporation_id=ANY($1) UNION ALL SELECT 'corp.sanction.expire:' || id::text FROM game.corp_vote_cases WHERE corporation_id=ANY($1))", &[&corporations]).await?;
    ex.execute("UPDATE game.corporations SET active_ceo_vote_id=NULL WHERE corporation_id=ANY($1)", &[&corporations]).await?;
    ex.execute("DELETE FROM game.corp_item_locks WHERE corporation_id=ANY($1)", &[&corporations]).await?;
    ex.execute("DELETE FROM game.corp_sanctioned_actions WHERE case_id IN (SELECT id FROM game.corp_vote_cases WHERE corporation_id=ANY($1))", &[&corporations]).await?;
    for table in ["corp_vote_claims", "corp_vote_ballots", "corp_vote_snapshot", "corp_vote_options"] {
        ex.execute(&format!("DELETE FROM game.{table} WHERE case_id IN (SELECT id FROM game.corp_vote_cases WHERE corporation_id=ANY($1))"), &[&corporations]).await?;
    }
    ex.execute("DELETE FROM game.corp_vote_cases WHERE corporation_id=ANY($1)", &[&corporations]).await?;
    Ok(())
}
