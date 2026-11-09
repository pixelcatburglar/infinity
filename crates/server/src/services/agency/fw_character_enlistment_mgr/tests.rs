use super::*;
use crate::services::{self, CallError};

async fn snapshot(db: &crate::db::Db, character: i64) -> String {
    db.query_one("SELECT (SELECT row_to_json(c)::text FROM game.characters c WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY job_id), '') FROM game.jobs j WHERE job_id LIKE '%' || $1::text || '%' OR payload LIKE '%' || $1::text || '%') AS state", &[&character]).await.unwrap().get("state")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn refusal_preserves_character_and_connection_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = services::tests::fixture::create(&db).await.unwrap();
    let foreign = services::tests::fixture::create(&db).await.unwrap();
    let corporation = services::tests::fixture::create_corporation(&db, fixture.character, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        services::tests::fixture::assign_corporation(&test_db, fixture.character, corporation, 0).await.unwrap();
        services::tests::fixture::seed_skill(&test_db, fixture.character).await.unwrap();
        for owner in [fixture.character, foreign.character] {
            crate::dao::items::transfers::insert(&test_db, 1, owner, owner, infinity_core::consts::FLAG_HANGAR, 3).await.unwrap();
        }
        let (mut client, peer) = services::tests::client(test_db.clone(), fixture.character).await;
        client.login.user_id = fixture.account;
        let args = [Value::Int(infinity_core::consts::FACTION_CALDARI)];
        client.session.apply(vec![("corpid", Value::Int(1_000_001))]);
        let empty_kwargs = Value::Dict(vec![]);
        let whitelist = Call { service: "fwCharacterEnlistmentMgr", method: "GetCorpAllowedEnlistmentFactions", args: &[], kwargs: &empty_kwargs, bound: false, bind_params: None };
        assert_eq!(services::dispatch(&mut client, &whitelist).await.unwrap(), Value::list(WARFARE_FACTIONS.map(Value::Int)));
        client.session.apply(vec![("corpid", Value::Int(corporation))]);
        let selected = [Value::list([Value::Int(infinity_core::consts::FACTION_CALDARI)])];
        let set = Call { service: "fwCharacterEnlistmentMgr", method: "SetMyCorpAllowedEnlistmentFactions", args: &selected, kwargs: &empty_kwargs, bound: false, bind_params: None };
        assert_eq!(services::dispatch(&mut client, &set).await.unwrap(), Value::None);
        let whitelist = Call { service: "fwCharacterEnlistmentMgr", method: "GetCorpAllowedEnlistmentFactions", args: &[], kwargs: &empty_kwargs, bound: false, bind_params: None };
        assert_eq!(services::dispatch(&mut client, &whitelist).await.unwrap(), selected[0]);
        let malformed = [Value::None, Value::Int(1)];
        let kwargs = Value::Dict(vec![(Value::str("unexpected"), Value::Bool(true))]);
        for role in [0, i64::MAX] {
            client.login.role = role;
            client.session.apply(vec![("role", Value::Int(role))]);
            for selected_character in [Some(fixture.character), None] {
            client.session.apply(vec![("charid", selected_character.map(Value::Int).unwrap_or(Value::None))]);
            for (method, arguments, kw) in [("CreateMyDirectEnlistment", &args[..], &empty_kwargs), ("RemoveMyDirectEnlistment", &[][..], &empty_kwargs), ("CreateMyDirectEnlistment", &malformed[..], &kwargs), ("RemoveMyDirectEnlistment", &malformed[..], &kwargs)] {
                let before = snapshot(&test_db, fixture.character).await;
                let foreign_before = snapshot(&test_db, foreign.character).await;
                let enlistment = services::dispatch(&mut client, &Call { service: "fwCharacterEnlistmentMgr", method: "GetMyEnlistment", args: &[], kwargs: &empty_kwargs, bound: false, bind_params: None }).await.unwrap();
                let cooldown = services::dispatch(&mut client, &Call { service: "fwCharacterEnlistmentMgr", method: "GetMyDirectEnlistmentCooldownTimestamp", args: &[], kwargs: &empty_kwargs, bound: false, bind_params: None }).await.unwrap();
                assert_eq!(enlistment, Value::tuple([Value::None, Value::None, Value::None]));
                assert_eq!(cooldown, Value::Int(0));
                let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
                let outbox = client.outbox.len();
                let call = Call { service: "fwCharacterEnlistmentMgr", method, args: arguments, kwargs: kw, bound: false, bind_params: None };
                let Err(CallError::User { msg, dict }) = services::dispatch(&mut client, &call).await else { panic!("expected refusal") };
                assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::faction_warfare::UNAVAILABLE))]);
                let error = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&error)).unwrap(), error);
                assert_eq!(snapshot(&test_db, fixture.character).await, before);
                assert_eq!(snapshot(&test_db, foreign.character).await, foreign_before);
                assert_eq!(services::dispatch(&mut client, &Call { service: "fwCharacterEnlistmentMgr", method: "GetMyEnlistment", args: &[], kwargs: &empty_kwargs, bound: false, bind_params: None }).await.unwrap(), enlistment);
                assert_eq!(services::dispatch(&mut client, &Call { service: "fwCharacterEnlistmentMgr", method: "GetMyDirectEnlistmentCooldownTimestamp", args: &[], kwargs: &empty_kwargs, bound: false, bind_params: None }).await.unwrap(), cooldown);
                assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                assert_eq!(client.outbox.len(), outbox);
                let mut byte = [0];
                assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            }
            }
        }
    }).await;
    db.execute("DELETE FROM game.corp_enlistment_factions WHERE corporation_id=$1", &[&corporation]).await.unwrap();
    services::tests::fixture::cleanup_corporation(&db, corporation, &[fixture.character], 0).await.unwrap();
    services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    services::tests::fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
