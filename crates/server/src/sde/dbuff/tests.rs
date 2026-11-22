use super::*;

fn row(key: i64, data: &str) -> dao::Row {
    dao::Row { key: Some(key.to_string()), data: data.to_string() }
}

const COLLECTION: &str = r#"{"_key": 7, "aggregateMode": "Minimum", "operationName": "ModAdd", "itemModifiers": [{"dogmaAttributeID": 9}], "locationModifiers": [{"dogmaAttributeID": 11}], "locationGroupModifiers": [{"groupID": 40, "dogmaAttributeID": 12}], "locationRequiredSkillModifiers": [{"skillID": 3300, "dogmaAttributeID": 13}]}"#;

fn modifiers(c: &DbuffCollection) -> Vec<(&str, i32, i32, Option<i32>, Option<i32>)> {
    c.modifiers.iter().map(|m| (m.modifier.func.as_str(), m.modifier.op, m.modifier.modified, m.modifier.group, m.modifier.skill)).collect()
}

#[test]
fn parses_all_four_selector_arrays() {
    let dbuffs = Dbuffs::from_rows(vec![row(7, COLLECTION)]).unwrap();
    let c = dbuffs.get(7).unwrap();
    assert_eq!(c.aggregate, DbuffAggregate::Minimum);
    assert!(c.modifiers.iter().all(|m| m.modifier.domain == "itemID"));
    assert_eq!(modifiers(c), [
        ("ItemModifier", fit::OP_MOD_ADD, 9, None, None),
        ("LocationModifier", fit::OP_MOD_ADD, 11, None, None),
        ("LocationGroupModifier", fit::OP_MOD_ADD, 12, Some(40), None),
        ("LocationRequiredSkillModifier", fit::OP_MOD_ADD, 13, None, Some(3300)),
    ]);
}

#[test]
fn rejects_malformed_rows() {
    for data in ["not json",
        r#"{"_key": 8, "aggregateMode": "Median", "operationName": "ModAdd"}"#,
        r#"{"_key": 8, "aggregateMode": "Maximum", "operationName": "Nope"}"#,
        r#"{"_key": 8, "aggregateMode": "Maximum", "operationName": "ModAdd", "itemModifiers": [{"dogmaAttributeID": -1}]}"#,
        r#"{"_key": 8, "aggregateMode": "Maximum", "operationName": "ModAdd", "locationGroupModifiers": [{"dogmaAttributeID": 5}]}"#] {
        assert!(Dbuffs::from_rows(vec![row(8, data)]).is_err(), "{data}");
    }
    assert!(Dbuffs::from_rows(vec![row(9, r#"{"_key": 8, "aggregateMode": "Maximum", "operationName": "ModAdd"}"#)]).is_err());
    assert!(Dbuffs::from_rows(vec![row(7, COLLECTION), row(7, COLLECTION)]).is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn loads_native_collections_and_expedition_selectors() {
    let (mut client, connection) = crate::config::tests::test_database().to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let dbuffs = Dbuffs::from_rows(crate::dao::sde::dbuff::load(&tx).await.unwrap()).unwrap();
    assert_eq!(modifiers(dbuffs.get(2464).unwrap()), [("LocationRequiredSkillModifier", fit::OP_POST_PERCENT, 6118, None, Some(3412))]);
    assert_eq!(modifiers(dbuffs.get(2465).unwrap()), [("ItemModifier", fit::OP_POST_PERCENT, 5796, None, None), ("LocationGroupModifier", fit::OP_POST_PERCENT, 54, Some(538), None), ("LocationGroupModifier", fit::OP_POST_PERCENT, 54, Some(1122), None)]);
    assert_eq!(modifiers(dbuffs.get(2466).unwrap()), [("LocationRequiredSkillModifier", fit::OP_POST_PERCENT, 6119, None, Some(3412))]);
    assert_eq!(modifiers(dbuffs.get(2468).unwrap()), [("LocationGroupModifier", fit::OP_MOD_ADD, 1909, Some(538), None)]);
    assert_eq!(modifiers(dbuffs.get(2481).unwrap()), [("LocationRequiredSkillModifier", fit::OP_POST_PERCENT, 73, None, Some(25863))]);
    tx.rollback().await.unwrap();
    drop(client);
    driver.await.unwrap();
}
