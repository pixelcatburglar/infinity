//! Scoped member updates, title blocking and atomicity on a dedicated database.
use infinity_core::{consts::{ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE_1}};
use crate::{db::Db, services::{corps::{permissions, roles}, tests::{self, fixture}}};
use marshal::Value;

fn patch(target: i64) -> Vec<Value> {
    let mut line = vec![Value::None; 15];
    line[0] = Value::Int(target);
    line
}

#[test]
fn member_rowset_respects_reordered_headers_and_zero() {
    let value = Value::instance("util.Rowset", [
        ("header", Value::list([Value::str("rolesAtHQ"), Value::str("characterID"), Value::str("titleMask")])),
        ("lines", Value::list([Value::list([Value::Int(0), Value::Int(42), Value::Int(0)])])),
    ]);
    let rows = roles::member_rowset(Some(&value)).unwrap();
    assert_eq!(rows[0][0], Value::Int(42));
    assert_eq!(rows[0][6], Value::Int(0));
    assert_eq!(rows[0][13], Value::Int(0));
    assert_eq!(rows[0][4], Value::None);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn scoped_grant_block_batch_rollback_and_observer() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let observer = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let corp: i64 = db.query_one("INSERT INTO game.corporations(name,ticker,ceo_id,creator_id,station_id,created) VALUES ('scoped-test-' || nextval('game.corporations_corporation_id_seq')::text,'SC' || currval('game.corporations_corporation_id_seq')::text,$1,$1,10,1) RETURNING corporation_id", &[&ceo.character]).await.unwrap().get("corporation_id");
    let ids = vec![ceo.character, manager.character, member.character, observer.character];
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=ANY($2)", &[&corp, &ids]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_grantable_hq=$2 WHERE character_id=$1", &[&manager.character, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (client, _peer) = tests::client(test_db.clone(), manager.character).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(observer.character, tx);
        let mut line = patch(member.character);
        line[6] = Value::Int(ROLE_HANGAR_CAN_TAKE_1);
        roles::update_member(&test_db, &client.world, &client.sde, manager.character, corp, &line).await.unwrap();
        let authority = permissions::load(&test_db, member.character).await.unwrap().unwrap();
        assert!(permissions::allows_at(&authority, corp, 10, ROLE_HANGAR_CAN_TAKE_1));
        assert!(!permissions::allows_at(&authority, corp, 20, ROLE_HANGAR_CAN_TAKE_1));
        assert!(rx.try_recv().is_ok_and(|(name, _)| name == "OnCorporationMemberChanged"));
        assert!(roles::update_member(&test_db, &client.world, &client.sde, manager.character, corp + 1, &line).await.is_err());
        let mut clear = patch(member.character);
        for index in 4..12 { clear[index] = Value::Int(0); }
        clear[13] = Value::Int(0);
        clear[14] = Value::Int(1);
        roles::update_member(&test_db, &client.world, &client.sde, member.character, corp, &clear).await.unwrap();
        assert!(roles::update_member(&test_db, &client.world, &client.sde, manager.character, corp, &line).await.is_err());
        let mut unblock = patch(member.character);
        unblock[14] = Value::Int(0);
        roles::update_member(&test_db, &client.world, &client.sde, member.character, corp, &unblock).await.unwrap();
        let before = permissions::load(&test_db, member.character).await.unwrap().unwrap().roles;
        let mut other = patch(outsider.character);
        other[6] = Value::Int(ROLE_HANGAR_CAN_TAKE_1);
        assert!(roles::update_members(&test_db, &client.world, &client.sde, manager.character, corp, vec![line.clone(), other], Vec::new()).await.is_err());
        assert_eq!(permissions::load(&test_db, member.character).await.unwrap().unwrap().roles, before);
        test_db.execute("UPDATE game.characters SET corp_block_roles=true,corp_title_mask=1 WHERE character_id=$1", &[&member.character]).await.unwrap();
        let title = vec![Value::Int(1), Value::unicode("Title"), Value::Int(0), Value::Int(0), Value::Int(ROLE_HANGAR_CAN_TAKE_1), Value::Int(0), Value::Int(0), Value::Int(0), Value::Int(0), Value::Int(0)];
        roles::update_titles(&test_db, &client.world, &client.sde, ceo.character, corp, vec![title]).await.unwrap();
        let blocked = permissions::load(&test_db, member.character).await.unwrap().unwrap();
        assert!(!permissions::allows_at(&blocked, corp, 10, ROLE_HANGAR_CAN_TAKE_1));
        while let Ok((name, _)) = rx.try_recv() { if name == "OnTitleChanged" { return; } }
        panic!("nonholder did not receive title change");
    }).await;
    db.execute("DELETE FROM game.corp_role_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0,corp_grantable_hq=0,corp_title_mask=0,corp_block_roles=false WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for person in [ceo, manager, member, observer, outsider] { fixture::cleanup(&db, person).await.unwrap(); }
    result.unwrap();
}
