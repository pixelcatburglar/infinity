use super::*;

#[test]
fn creation_quantity_accepts_exact_shorthand_and_preserves_boundaries() {
    assert_eq!(create_quantity(None).unwrap(), 1);
    for value in [1_i64, 1_i64 << 31, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
        assert_eq!(create_quantity(Some(&value.to_string())).unwrap(), value);
    }
    for (text, expected) in [
        ("1k", 1_000), ("1m", 1_000_000), ("1B", 1_000_000_000),
        ("1t", 1_000_000_000_000), ("1.5m", 1_500_000),
        ("2.25b", 2_250_000_000), ("0.001k", 1), ("1.0000k", 1_000),
        ("0.001000000000000000000k", 1), ("9223372036854775.807k", i64::MAX),
    ] {
        assert_eq!(create_quantity(Some(text)).unwrap(), expected, "{text}");
    }
    for text in [
        "", "0", "-1", "+1", "1,000", "1.0", " 1", "１", "9223372036854775808", "x",
        "9223373t", "1.0001k", "0k", "-1m", "m", "1mm", "1.m", ".5m",
        "9223372036854775.808k", " 1k", "1k ",
    ] {
        assert!(create_quantity(Some(text)).is_err(), "{text}");
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn create_aliases_preserve_total_and_reject_invalid_without_side_effects() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    client.login.role = GM_ROLES;
    let station = client.sde.map.stations().next().expect("test SDE has a station").station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let ty = client.sde.types.iter().find(|ty| ty.name.is_some() && ty.published == Some(true)).unwrap().type_id;
    for (command, token, qty) in [
        ("create", (i64::MAX - 3).to_string(), i64::MAX - 3),
        ("createitem", "10".to_string(), 10),
        ("create", "1k".to_string(), 1_000),
    ] {
        let reply = isk_command(&mut client, command, &format!("{ty} {token}")).await.unwrap();
        assert!(reply.as_text().unwrap().contains(&format!("created {qty} x")));
    }
    let total: String = db.query_one("SELECT COALESCE(SUM(quantity),0)::text AS total FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&fixture.character, &ty]).await.unwrap().get("total");
    assert_eq!(total.parse::<i128>().unwrap(), i128::from(i64::MAX) + 1007);
    client.outbox.clear();
    for command in ["create", "createitem"] {
        assert!(isk_command(&mut client, command, &format!("{ty} 0")).await.is_err());
    }
    let after: String = db.query_one("SELECT COALESCE(SUM(quantity),0)::text AS total FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&fixture.character, &ty]).await.unwrap().get("total");
    assert_eq!(after, total);
    assert!(client.outbox.is_empty());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
