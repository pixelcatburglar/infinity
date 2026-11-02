use super::*;
use crate::services::tests::{client, fixture};
use std::collections::BTreeMap;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn epic_journal_dispatch_matches_loaded_rows_for_bound_and_unbound_calls() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let mut expected: BTreeMap<String, BTreeMap<i32, i64>> = BTreeMap::new();
        for row in c.sde.missions.journal() {
            expected.entry(row.key.clone()).or_default().insert(row.mission_id, row.message_id.expect("journal message ID"));
        }
        assert!(!expected.is_empty(), "dedicated SDE must contain journal messages");
        let expected = Value::Dict(expected.into_iter().map(|(key, rows)| (Value::str(key), Value::Dict(rows.into_iter().map(|(id, msg)| (Value::Int(i64::from(id)), Value::Int(msg))).collect()))).collect());
        let agent_id = super::super::agents().keys().next().copied().expect("retail agent");
        let bind = Value::Int(agent_id);
        let kwargs = Value::None;
        let before = fixture::mission_fingerprint(&test_db, f.character).await.unwrap();
        for bound in [false, true] {
            let call = Call { service: "agentMgr", method: "GetMessagesForEpicArcMissions", args: &[], kwargs: &kwargs, bound, bind_params: bound.then_some(&bind) };
            assert!(crate::services::agency::dispatch(&mut c, &call).await.is_none());
            let leaf = dispatch(&mut c, &call).await.unwrap().unwrap();
            let top = crate::services::dispatch(&mut c, &call).await.unwrap();
            assert_eq!(leaf, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&top)).unwrap(), expected);
        }
        assert_eq!(fixture::mission_fingerprint(&test_db, f.character).await.unwrap(), before);
        assert!(c.outbox.is_empty());
    }).await;
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "journal task failed: {result:?}");
}
