use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn immutable_links_have_unique_ids_and_full_key_lookup() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('shared-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"test"[..]]).await.unwrap().get("user_id");
    let mut ids = Vec::new();
    let result: anyhow::Result<()> = async {
        let new = NewLink { setting_type_id: 2, hash_value: "digest", payload: "first", creator_user_id: account };
        let (a, b) = tokio::join!(insert(&db, &new), insert(&db, &new));
        if let Ok(row) = &a { ids.push(row.sq_id); }
        if let Ok(row) = &b { ids.push(row.sq_id); }
        let (a, b) = (a?.sq_id, b?.sq_id);
        let key = LinkKey { sq_id: a, hash_value: "digest", setting_type_id: 2 };
        anyhow::ensure!(a != b, "concurrent inserts reused an ID");
        anyhow::ensure!(get(&db, &key).await?.is_some_and(|row| row.payload == "first"), "initial snapshot missing");
        anyhow::ensure!(get(&db, &LinkKey { hash_value: "wrong", ..key }).await?.is_none(), "wrong hash resolved");
        anyhow::ensure!(get(&db, &LinkKey { sq_id: a, hash_value: "digest", setting_type_id: 3 }).await?.is_none(), "wrong type resolved");
        let changed = NewLink { setting_type_id: 2, hash_value: "changed", payload: "second", creator_user_id: account };
        let changed_id = insert(&db, &changed).await?.sq_id;
        ids.push(changed_id);
        anyhow::ensure!(get(&db, &key).await?.is_some_and(|row| row.payload == "first"), "old snapshot changed");
        let reopened = crate::db::Db::connect(&crate::config::tests::test_database()).await?;
        anyhow::ensure!(get(&reopened, &key).await?.is_some_and(|row| row.payload == "first"), "old snapshot did not persist");
        let overview: i64 = db.query_one("INSERT INTO game.shared_setting_links (kind, setting_type_id, hash_value, payload, creator_user_id) VALUES ('overview', NULL, 'digest', 'overview', $1) RETURNING sq_id", &[&account]).await?.get("sq_id");
        ids.push(overview);
        anyhow::ensure!(get(&db, &LinkKey { sq_id: overview, hash_value: "digest", setting_type_id: 2 }).await?.is_none(), "wrong namespace resolved");
        Ok(())
    }.await;
    db.execute("DELETE FROM game.shared_setting_links WHERE sq_id = ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}
