//! Augmentation read wire shape and active membership regressions.
use super::*;
use infinity_core::consts::{FILETIME_MINUTE, FILETIME_SECOND};
use crate::services::tests::{client, fixture};

async fn insert(db: &crate::db::Db, character: i64, type_id: i32, flag: i32, start: Option<i64>, end: Option<i64>, effects: Option<Vec<i32>>) -> i64 {
    db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,$5,$6) RETURNING item_id", &[&type_id, &character, &flag, &start, &end, &effects]).await.unwrap().get("item_id")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn augmentation_reads_preserve_identity_metadata_and_active_membership() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let now = filetime_now();
    let activation = now - FILETIME_SECOND;
    let expiry = now + FILETIME_MINUTE;
    let past = now - FILETIME_SECOND;
    let type_id = 1_i32;
    let implant = insert(&db, f.character, type_id, FLAG_IMPLANT, None, None, None).await;
    let booster = insert(&db, f.character, type_id, FLAG_BOOSTER, Some(activation), Some(expiry), Some(vec![9, 3])).await;
    let expired = insert(&db, f.character, type_id, FLAG_BOOSTER, None, Some(past), None).await;
    let legacy = insert(&db, f.character, type_id, FLAG_BOOSTER, None, None, None).await;
    let implants = get_implants(&c, f.character).await.unwrap_or_else(|_| panic!("implant read failed"));
    assert_eq!(implants, Value::Dict(vec![(Value::Int(implant), keyval([
        ("itemID", Value::Int(implant)), ("typeID", Value::Int(i64::from(type_id))),
    ]))]));
    let boosters = get_boosters(&c, f.character).await.unwrap_or_else(|_| panic!("booster read failed"));
    assert_eq!(boosters, Value::Dict(vec![
        (Value::Int(booster), keyval([
            ("itemID", Value::Int(booster)), ("boosterTypeID", Value::Int(i64::from(type_id))),
            ("expiryTime", Value::Int(expiry)), ("boosterDuration", Value::Int(ms_from_filetime_ticks(expiry - activation))),
            ("sideEffectIDs", Value::list([Value::Int(3), Value::Int(9)])),
        ])),
        (Value::Int(legacy), keyval([
            ("itemID", Value::Int(legacy)), ("boosterTypeID", Value::Int(i64::from(type_id))),
            ("expiryTime", Value::None), ("boosterDuration", Value::None), ("sideEffectIDs", Value::list([])),
        ])),
    ]));
    assert_ne!(expired, booster);
    fixture::cleanup(&db, f).await.unwrap();
}
