use super::*;

#[test]
fn visibility_uses_the_selected_region() {
    let skin = Skin { skin_id: 1, type_ids: vec![], is_structure_skin: false, visible_serenity: true, visible_tranquility: false };
    assert!(skin.visible("optic"));
    assert!(!skin.visible("tranquility"));
    assert!(!skin.visible("other"));
    let other = Skin { visible_serenity: false, visible_tranquility: true, ..skin };
    assert!(!other.visible("optic"));
    assert!(other.visible("tranquility"));
    assert!(other.visible("other"));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with native SDE"]
async fn native_rows_load_and_preserve_license_references() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();

    // Pick negative keys and row numbers absent from both native tables. The primary
    // keys still protect against a concurrent insertion; all writes are rolled back.
    let rows = tx.query("SELECT id FROM (SELECT DISTINCT (-1 - floor(random() * 2000000000))::int AS id FROM generate_series(1, 100)) candidates WHERE NOT EXISTS (SELECT 1 FROM sde_native.skins WHERE row_no = id OR key = id::text) AND NOT EXISTS (SELECT 1 FROM sde_native.skinlicenses WHERE row_no = id OR key = id::text) LIMIT 2", &[]).await.unwrap();
    assert_eq!(rows.len(), 2, "could not allocate synthetic native keys");
    let a: i32 = rows[0].get("id");
    let b: i32 = rows[1].get("id");
    tx.execute("INSERT INTO sde_native.skins (row_no, key, data) VALUES ($1, $1::int::text, jsonb_build_object('types', jsonb_build_array($2::int, $1::int, $2::int))), ($2, $2::int::text, jsonb_build_object('types', jsonb_build_array($1::int), 'isStructureSkin', true, 'visibleSerenity', true, 'visibleTranquility', false))", &[&a, &b]).await.unwrap();
    tx.execute("INSERT INTO sde_native.skinlicenses (row_no, key, data) VALUES ($1, $1::int::text, jsonb_build_object('skinID', $2::int, 'duration', 12)), ($2, $2::int::text, jsonb_build_object('skinID', $1::int, 'duration', -1, 'isSingleUse', true))", &[&a, &b]).await.unwrap();

    let data = ShipSkins::load(&tx).await.unwrap();
    let missing_flags = data.skin(a).unwrap();
    assert_eq!(missing_flags.type_ids, vec![b, a, b]);
    assert!(!missing_flags.is_structure_skin);
    assert!(!missing_flags.visible("optic"));
    assert!(!missing_flags.visible("tranquility"));
    let explicit_flags = data.skin(b).unwrap();
    assert_eq!(explicit_flags.type_ids, vec![a]);
    assert!(explicit_flags.is_structure_skin);
    assert!(explicit_flags.visible("optic"));
    assert!(!explicit_flags.visible("tranquility"));
    let default_license = data.licenses().find(|license| license.license_type_id == a).unwrap();
    assert_eq!(default_license.skin_id, b);
    assert_eq!(default_license.duration_days, 12);
    assert!(!default_license.is_single_use);
    let explicit_license = data.licenses().find(|license| license.license_type_id == b).unwrap();
    assert_eq!(explicit_license.skin_id, a);
    assert_eq!(explicit_license.duration_days, -1);
    assert!(explicit_license.is_single_use);
    tx.rollback().await.unwrap();
}
