use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn tournament_setup_dispatch_preserves_keyed_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = super::client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
        let owners = [owner.character, foreign.character];
        let before = fixture::market_fingerprint(&test_db, &owners, &owners).await.unwrap();
        let owner_state = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let foreign_state = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let kwargs = Value::Dict(vec![]);
        let call = Call { service: "tourneyMgr", method: "GetTourneySetup", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let expected = Value::Tuple(vec![Value::Dict(vec![]), Value::Dict(vec![]), Value::List(vec![])]);
        assert_eq!(fixed::root(&call).unwrap().unwrap(), expected);
        for docked in [true, false] {
            client.session.apply(vec![("stationid", if docked { Value::Int(60003760) } else { Value::None }), ("solarsystemid", if docked { Value::None } else { Value::Int(30000142) })]);
            let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            let sid = client.session.sid;
            let login = (client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone());
            let bound = client.bound.clone();
            let pilots = client.world.online_pilots();
            let reply = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(reply, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
            assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
            assert_eq!(client.session.sid, sid);
            assert_eq!((client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone()), login);
            assert_eq!(client.bound, bound);
            assert_eq!(client.world.online_pilots(), pilots);
            assert!(client.outbox.is_empty());
            assert_eq!(fixture::market_fingerprint(&test_db, &owners, &owners).await.unwrap(), before);
            assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), owner_state);
            assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), foreign_state);
            let mut bytes = [0u8; 1];
            assert!(matches!(peer.try_read(&mut bytes), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
