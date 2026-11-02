//! Redeem token wire forms and transactional delivery.
use super::*;
use crate::db::outbox::{Effect, Notification, Outbox};

#[test]
fn token_keys_roundtrip() {
    let keys = Value::list([
        Value::tuple([Value::Int(1), Value::None]),
        Value::list([Value::Int(2), Value::None]),
        Value::dict([("tokenID", Value::Int(3))]),
        keyval([("tokenID", Value::Int(4))]),
    ]);
    let keys = marshal::loads(&marshal::dumps(&keys)).unwrap();
    assert_eq!(token_ids(Some(&keys)), vec![1, 2, 3, 4]);
    let error = redeem_error(4);
    assert_eq!(marshal::loads(&marshal::dumps(&error)).unwrap(), error);
    assert_eq!(error.items().unwrap()[1], crate::client::user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::store::REDEEM_FAILED))]));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn partial_claim_commit_and_rollback() {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let cfg = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&cfg).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let type_id = sde.types.iter().find(|t| !SKILL_POINT_TYPES.iter().any(|(id, _)| *id == t.type_id)).unwrap().type_id;
    let sp_type = SKILL_POINT_TYPES.iter().find(|(id, _)| sde.types.get(*id).is_some()).unwrap().0;
    let sp_amount = SKILL_POINT_TYPES.iter().find(|(id, _)| *id == sp_type).unwrap().1;
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let user: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('redeem-fixture-' || nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let foreign_user: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('redeem-fixture-' || nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let foreign_token: i64 = tx.query_one("INSERT INTO game.redeem_tokens (user_id,type_id,quantity,label,created) VALUES ($1,$2,1,'fixture',0) RETURNING token_id", &[&foreign_user, &type_id]).await.unwrap().get("token_id");
    let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,home_station_id,solar_system_id) VALUES ($1,'redeem-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,$2,1) RETURNING character_id", &[&user, &station]).await.unwrap().get("character_id");
    let mut ids = Vec::new();
    for ty in [type_id, i32::MIN, sp_type, type_id, sp_type] {
        let id: i64 = tx.query_one("INSERT INTO game.redeem_tokens (user_id,type_id,quantity,label,created) VALUES ($1,$2,1,'fixture',0) RETURNING token_id", &[&user, &ty]).await.unwrap().get("token_id");
        ids.push(id);
    }
    tx.commit().await.unwrap();

    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let rolled_back: CallResult = db.transaction(
        |tx, outbox: &mut Outbox| {
            let ids = ids.clone();
            let sde = sde.clone();
            Box::pin(async move {
                claim_in_transaction(tx, outbox, &sde, user, character, &ids[..3], false, true, Some(station)).await?;
                Err(db_err(anyhow::anyhow!("fixture rollback")))
            })
        },
        move |value, _outbox| async move { flag.store(true, Ordering::SeqCst); Ok(value) },
    ).await;
    assert!(rolled_back.is_err());
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(crate::dao::store::purchases::tokens(&db, user).await.unwrap().len(), 5);

    let initial_points: i64 = client.query_one("SELECT free_skill_points FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap().get("free_skill_points");
    let unauthorized: CallResult = db.transaction(
        |tx, outbox: &mut Outbox| {
            let ids = ids.clone();
            let sde = sde.clone();
            Box::pin(async move { claim_in_transaction(tx, outbox, &sde, foreign_user, character, &ids, false, false, None).await })
        },
        |value, _outbox| async move { Ok(value) },
    ).await;
    assert!(unauthorized.is_err());

    let dispatch_db = db.clone();
    let (value, effects): (Value, Vec<Effect<std::convert::Infallible>>) = db.transaction(
        |tx, outbox: &mut Outbox| {
            let claim_ids = vec![ids[0], ids[1], ids[2], ids[0], foreign_token];
            let sde = sde.clone();
            Box::pin(async move { claim_in_transaction(tx, outbox, &sde, user, character, &claim_ids, false, true, Some(station)).await })
        },
        |value, outbox: Outbox| async move {
            assert_eq!(crate::dao::store::purchases::tokens(&dispatch_db, user).await.unwrap().len(), 3);
            Ok::<_, CallError>((value, outbox.into_entries()))
        },
    ).await.unwrap();
    let items = value.get("itemsCreated").unwrap().items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].get("stationID"), Some(&Value::Int(station)));
    let item_id = items[0].get("itemID").unwrap().as_int().unwrap();
    let errors = value.get("errors").unwrap().items().unwrap();
    assert_eq!(errors, &[redeem_error(ids[1])]);
    assert_eq!(marshal::loads(&marshal::dumps(&errors[0])).unwrap(), errors[0]);
    let foreign_remaining = crate::dao::store::purchases::tokens(&db, foreign_user).await.unwrap();
    assert_eq!(foreign_remaining.iter().map(|t| t.token_id).collect::<Vec<_>>(), vec![foreign_token]);
    assert!(effects.iter().any(|e| matches!(e, Effect::Notification(Notification::DirectClient { event, .. }) if event == "OnItemChange")));
    assert!(effects.iter().any(|e| matches!(e, Effect::Notification(Notification::DirectClient { event, .. }) if event == "OnFreeSkillPointsChanged")));
    let points: i64 = client.query_one("SELECT free_skill_points FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap().get("free_skill_points");
    assert_eq!(points, initial_points + sp_amount);

    let (value, effects): (Value, Vec<Effect<std::convert::Infallible>>) = db.transaction(
        |tx, outbox: &mut Outbox| {
            let claim_ids = vec![ids[3], ids[4]];
            let sde = sde.clone();
            Box::pin(async move { claim_in_transaction(tx, outbox, &sde, user, character, &claim_ids, false, true, Some(-1)).await })
        },
        |value, outbox: Outbox| async move { Ok::<_, CallError>((value, outbox.into_entries())) },
    ).await.unwrap();
    assert_eq!(value.get("errors").unwrap().items().unwrap(), &[redeem_error(ids[3])]);
    assert!(value.get("itemsCreated").unwrap().items().unwrap().is_empty());
    assert!(effects.iter().any(|e| matches!(e, Effect::Notification(Notification::DirectClient { event, .. }) if event == "OnFreeSkillPointsChanged")));
    let remaining = crate::dao::store::purchases::tokens(&db, user).await.unwrap();
    assert_eq!(remaining.iter().map(|t| t.token_id).collect::<Vec<_>>(), vec![ids[1], ids[3]]);

    client.execute("DELETE FROM game.items WHERE item_id=$1", &[&item_id]).await.unwrap();
    let points: i64 = client.query_one("SELECT free_skill_points FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap().get("free_skill_points");
    assert_eq!(points, initial_points + 2 * sp_amount);
    client.execute("DELETE FROM game.redeem_tokens WHERE user_id=$1", &[&user]).await.unwrap();
    client.execute("DELETE FROM game.redeem_tokens WHERE user_id=$1", &[&foreign_user]).await.unwrap();
    client.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    client.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&user]).await.unwrap();
    client.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&foreign_user]).await.unwrap();
}
