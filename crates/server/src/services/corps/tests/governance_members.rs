//! Client member-query grammar regressions.
use crate::services::corps::members;
use marshal::Value;

#[test]
fn query_grammar_checks_join_property_operator_and_types() {
    let valid = Value::list([
        Value::list([Value::str("rolesAtHQ"), Value::Int(7), Value::Int(1_i64 << 55)]),
        Value::list([Value::Int(2), Value::str("characterID"), Value::Int(11), Value::unicode("Pilot_%")]),
        Value::list([Value::Int(1), Value::str("baseID"), Value::Int(6), Value::Int(600001)]),
    ]);
    let clauses = members::parse(Some(&valid)).unwrap();
    assert_eq!(clauses.len(), 3);
    assert_eq!(clauses[1].join, 2);
    assert_eq!(clauses[2].join, 1);
    let invalid = Value::list([Value::list([Value::str("roles"), Value::Int(1), Value::Int(1)])]);
    assert!(members::parse(Some(&invalid)).is_err());
    let invalid_join = Value::list([Value::list([Value::str("baseID"), Value::Int(1), Value::Int(1)]), Value::list([Value::Int(7), Value::str("baseID"), Value::Int(1), Value::Int(2)])]);
    assert!(members::parse(Some(&invalid_join)).is_err());
    let invalid_type = Value::list([Value::list([Value::str("characterID"), Value::Int(9), Value::Int(123)])]);
    assert!(members::parse(Some(&invalid_type)).is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn paged_rpc_and_query_flags_use_current_members() {
    use infinity_core::consts::ROLE_DIRECTOR;
    use crate::{db::Db, services::{Call, tests::{self, fixture}}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut people = Vec::new();
    for _ in 0..101 { people.push(fixture::create(&db).await.unwrap()); }
    let ceo = people[0].character;
    let direct = people[1].character;
    let titled = people[2].character;
    let blocked = people[3].character;
    let corp = fixture::create_corporation(&db, ceo, 60000001).await.unwrap();
    let ids: Vec<i64> = people.iter().map(|person| person.character).collect();
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=ANY($2)", &[&corp, &ids]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo, &ROLE_DIRECTOR]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo).await;
        let valid = client.sde.corporation_roles.scope_masks();
        let mask = 1_i64 << (63 - valid[1].leading_zeros());
        test_db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&direct, &mask]).await.unwrap();
        test_db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles_hq) VALUES ($1,1,'query-title',$2)", &[&corp, &mask]).await.unwrap();
        test_db.execute("UPDATE game.characters SET corp_title_mask=1 WHERE character_id=ANY($1)", &[&vec![titled, blocked]]).await.unwrap();
        test_db.execute("UPDATE game.characters SET corp_block_roles=true WHERE character_id=$1", &[&blocked]).await.unwrap();
        let mut metadata = vec![Value::None; 15];
        metadata[0] = Value::Int(titled);
        metadata[2] = Value::Int(3);
        assert!(super::super::roles::update_member(&test_db, &client.world, &client.sde, direct, corp, &metadata).await.is_err());
        metadata[0] = Value::Int(ceo);
        assert!(super::super::roles::update_member(&test_db, &client.world, &client.sde, direct, corp, &metadata).await.is_err());
        let criteria = Value::list([Value::list([Value::str("rolesAtHQ"), Value::Int(7), Value::Int(mask)])]);
        let clauses = members::parse(Some(&criteria)).unwrap();
        for (implied, titles, expected) in [(false,false,vec![direct]), (true,false,vec![ceo,direct]), (false,true,vec![direct,titled]), (true,true,vec![ceo,direct,titled])] {
            let found = crate::dao::corporations::members::query(&test_db, corp, &clauses, implied, titles, valid).await.unwrap();
            let mut expected = expected;
            expected.sort_unstable();
            assert_eq!(found, expected);
            assert!(!found.contains(&blocked));
        }
        let query_args = [criteria, Value::Bool(true), Value::Bool(true)];
        let kwargs = Value::Dict(vec![]);
        let binding = Value::Int(corp);
        let query_call = Call { service: "corpRegistry", method: "GetMemberIDsByQuery", args: &query_args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
        let result = super::super::dispatch(&mut client, &query_call).await.unwrap().unwrap();
        let result = marshal::loads(&marshal::dumps(&result)).unwrap();
        let mut expected = vec![ceo, direct, titled];
        expected.sort_unstable();
        assert_eq!(result, Value::list(expected.into_iter().map(Value::Int)));
        let joined = Value::list([
            Value::list([Value::str("baseID"), Value::Int(1), Value::Int(60000001)]),
            Value::list([Value::Int(2), Value::str("startDateTime"), Value::Int(2), Value::Int(0)]),
            Value::list([Value::Int(1), Value::str("rolesAtHQ"), Value::Int(7), Value::Int(mask)]),
        ]);
        let parsed = members::parse(Some(&joined)).unwrap();
        let found = crate::dao::corporations::members::query(&test_db, corp, &parsed, false, false, valid).await.unwrap();
        assert_eq!(found, vec![direct]);
        for (operator, expected_count) in [(9, 0), (10, 101)] {
            let names = Value::list([Value::list([Value::str("characterID"), Value::Int(operator), Value::unicode("atomicity-%")])]);
            let parsed = members::parse(Some(&names)).unwrap();
            assert_eq!(crate::dao::corporations::members::query(&test_db, corp, &parsed, false, false, valid).await.unwrap().len(), expected_count);
        }
        let mut seen = Vec::new();
        for page in 1..=3 {
            let args = [Value::Int(page)];
            let kwargs = Value::Dict(vec![]);
            let binding = Value::Int(corp);
            let call = Call { service: "corpRegistry", method: "GetMembersPaged", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
            let result = super::super::dispatch(&mut client, &call).await.unwrap().unwrap();
            let result = marshal::loads(&marshal::dumps(&result)).unwrap();
            let Value::Instance(class, state) = result else { panic!("expected paged result set") };
            assert_eq!(*class, Value::str("eve.common.script.util.pagedCollection.PagedResultSet"));
            assert_eq!(state.get("totalCount"), Some(&Value::Int(101)));
            assert_eq!(state.get("page"), Some(&Value::Int(page)));
            assert_eq!(state.get("perPage"), Some(&Value::Int(50)));
            let rows = state.get("collection").unwrap().items().unwrap();
            assert_eq!(rows.len(), if page < 3 { 50 } else { 1 });
            for row in rows {
                let Value::Instance(_, state) = row else { panic!("expected member row") };
                seen.push(state.get("characterID").unwrap().as_int().unwrap());
            }
        }
        let mut expected = ids.clone();
        expected.sort_unstable();
        assert_eq!(seen, expected);
        test_db.execute("UPDATE game.characters SET observed_logoff_at=now()-interval '2 hours' WHERE character_id=$1", &[&direct]).await.unwrap();
        client.session.apply(vec![("charid", Value::Int(direct))]);
        let args = [];
        let kwargs = Value::Dict(vec![]);
        let binding = Value::Int(corp);
        let detailed = Call { service: "corpRegistry", method: "GetMemberTrackingInfo", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
        assert!(super::super::dispatch(&mut client, &detailed).await.unwrap().is_err());
        let simple = Call { method: "GetMemberTrackingInfoSimple", ..detailed };
        let Value::Instance(_, state) = super::super::dispatch(&mut client, &simple).await.unwrap().unwrap() else { panic!("expected tracking rowset") };
        let header = state.get("header").unwrap().items().unwrap();
        assert!(!header.contains(&Value::str("locationID")));
        assert!(!header.contains(&Value::str("shipTypeID")));
        let lines = state.get("lines").unwrap().items().unwrap();
        let row = lines.iter().map(|line| line.items().unwrap()).find(|line| line[0] == Value::Int(direct)).unwrap();
        let Value::Float(hours) = &row[1] else { panic!("expected elapsed hours") };
        assert!((2.0..3.0).contains(hours));
    }).await;
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0,corp_title_mask=0,corp_block_roles=false WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for person in people { fixture::cleanup(&db, person).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn bulk_role_and_title_removals_share_stored_operation_times() {
    use infinity_core::{consts::{ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE_1}};
    use crate::{db::Db, services::tests::{self, fixture}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let first = fixture::create(&db).await.unwrap();
    let second = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [first.character, second.character];
    let all = [ceo.character, first.character, second.character];
    db.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=0,corp_roles_hq=0 WHERE character_id=ANY($2)", &[&corp, &all.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (client, _) = tests::client(test_db.clone(), ceo.character).await;
        test_db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=ANY($1)", &[&ids.as_slice(), &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        let lines = ids.iter().map(|&id| {
            let mut line = vec![Value::None; 15];
            line[0] = Value::Int(id);
            line[6] = Value::Int(0);
            line
        }).collect();
        super::super::roles::update_members(&test_db, &client.world, &client.sde, ceo.character, corp, lines, vec![]).await.unwrap();
        let rows = test_db.query("SELECT c.corp_roles_removed_at,h.change_time FROM game.characters c JOIN game.corp_role_history h ON h.character_id=c.character_id WHERE c.character_id=ANY($1) AND h.corporation_id=$2", &[&ids.as_slice(), &corp]).await.unwrap();
        assert_eq!(rows.len(), 2);
        let stamp: i64 = rows[0].get("change_time");
        for row in rows { assert_eq!(row.get::<_, i64>("change_time"), stamp); assert_eq!(row.get::<_, i64>("corp_roles_removed_at"), stamp); }

        test_db.execute("DELETE FROM game.corp_role_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
        test_db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles_hq) VALUES ($1,1,'shared',$2)", &[&corp, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        test_db.execute("UPDATE game.characters SET corp_title_mask=1,corp_roles_removed_at=NULL WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
        super::super::roles::delete_title(&test_db, &client.world, &client.sde, ceo.character, corp, 1).await.unwrap();
        let rows = test_db.query("SELECT c.corp_roles_removed_at,h.change_time FROM game.characters c JOIN game.corp_role_history h ON h.character_id=c.character_id WHERE c.character_id=ANY($1) AND h.corporation_id=$2", &[&ids.as_slice(), &corp]).await.unwrap();
        assert_eq!(rows.len(), 2);
        let stamp: i64 = rows[0].get("change_time");
        for row in rows { assert_eq!(row.get::<_, i64>("change_time"), stamp); assert_eq!(row.get::<_, i64>("corp_roles_removed_at"), stamp); }
    }).await;
    db.execute("DELETE FROM game.corp_role_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0,corp_title_mask=0 WHERE character_id=ANY($1)", &[&all.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for person in [ceo, first, second] { fixture::cleanup(&db, person).await.unwrap(); }
    result.unwrap();
}
