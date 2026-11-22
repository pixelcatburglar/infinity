use super::price_value;

#[test]
fn insurance_price_prefers_nonzero_retail_then_nullable_base() {
    assert_eq!(price_value(Some(7.0), Some(8.0), Some(9.0)), 7.0);
    assert_eq!(price_value(Some(0.0), Some(8.0), Some(9.0)), 8.0);
    assert_eq!(price_value(None, Some(0.0), Some(9.0)), 9.0);
    assert_eq!(price_value(None, None, Some(0.0)), 0.0);
    assert_eq!(price_value(None, None, None), 0.0);
    assert_eq!(price_value(Some(-1.0), Some(8.0), Some(9.0)), -1.0);
    assert!(price_value(Some(f64::NAN), Some(8.0), Some(9.0)).is_nan());
    assert!(price_value(Some(0.0), Some(f64::NAN), Some(9.0)).is_nan());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn insure_refuses_when_the_locked_hull_changes_owner() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db_cfg = cfg.clone();
    let (mut setup, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..db_cfg }).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let kind = sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && ty.published == Some(true) && super::value(&sde, ty.type_id) > 0.0).map(|ty| ty.type_id).min().expect("insurable ship");
    let amount = super::value(&sde, kind) * (super::FRACTIONS[0] - super::BASE_FRACTION) * super::FRACTION_INCREMENT;
    let tx = setup.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('insurance-race-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'insurance-race-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let other: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'insurance-race-other-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let ship: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($2,$1,1,0,1,true) RETURNING item_id", &[&owner, &kind]).await.unwrap().get("item_id");
    tx.commit().await.unwrap();
    let raced = tokio::spawn(async move {
        let before_balance: String = setup.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
        let before_notifications: i64 = setup.query_one("SELECT count(*) AS count FROM game.notifications WHERE receiver_id=$1", &[&owner]).await.unwrap().get("count");
        let holder = setup.transaction().await.unwrap();
        let holder_pid: i32 = holder.query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get("pg_backend_pid");
        assert!(crate::dao::items::ship::lock_reload_ship(&holder, ship).await.unwrap());
        let rpc_db = db.clone();
        let rpc = tokio::spawn(async move {
            let (mut client, _peer) = crate::services::tests::client(rpc_db, owner).await;
            client.session.apply(vec![("userid", marshal::Value::Int(account)), ("charid", marshal::Value::Int(owner))]);
            let args = [marshal::Value::Int(ship), marshal::Value::Float(amount)];
            let call = crate::services::Call { service: "insuranceSvc", method: "InsureShip", args: &args, kwargs: &marshal::Value::None, bound: true, bind_params: None };
            let result = super::dispatch(&mut client, &call).await;
            (result, client.outbox.len())
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: bool = holder.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND $1 = ANY(pg_blocking_pids(pid))) AS waiting", &[&holder_pid]).await.unwrap().get("waiting");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("insure did not reach the hull lock");
        holder.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&ship, &other]).await.unwrap();
        holder.commit().await.unwrap();
        let (refusal, queued) = rpc.await.unwrap();
        let refusal = refusal.expect("insurance dispatch");
        assert!(matches!(&refusal, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict.as_slice() == [("notify".into(), marshal::Value::unicode(infinity_core::messages::INSURANCE_REQUIRES_OWN_ASSEMBLED_SHIP))]), "unexpected insurance refusal: {refusal:?}");
        assert_eq!(queued, 0, "a refusal must publish nothing");
        assert!(setup.query_opt("SELECT ship_id FROM game.insurance WHERE ship_id=$1", &[&ship]).await.unwrap().is_none());
        let after_balance: String = setup.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
        assert_eq!(after_balance, before_balance, "a refusal must not debit");
        let after_notifications: i64 = setup.query_one("SELECT count(*) AS count FROM game.notifications WHERE receiver_id=$1", &[&owner]).await.unwrap().get("count");
        assert_eq!(after_notifications, before_notifications, "a refusal must publish no notification");
        crate::dao::insurance::insure(&db, ship, other, kind, super::FRACTIONS[0], 1, i64::MAX).await.unwrap();
        let args = [marshal::Value::Int(ship)];
        let uninsure = crate::services::Call { service: "insuranceSvc", method: "UnInsureShip", args: &args, kwargs: &marshal::Value::None, bound: true, bind_params: None };
        let (mut client, _peer) = crate::services::tests::client(db.clone(), owner).await;
        client.session.apply(vec![("userid", marshal::Value::Int(account)), ("charid", marshal::Value::Int(owner))]);
        assert!(matches!(super::dispatch(&mut client, &uninsure).await.unwrap(), Ok(marshal::Value::None)));
        assert!(setup.query_opt("SELECT ship_id FROM game.insurance WHERE ship_id=$1 AND owner_id=$2", &[&ship, &other]).await.unwrap().is_some(), "the other owner's policy stays");
        let (mut owner_client, _peer) = crate::services::tests::client(db.clone(), other).await;
        owner_client.session.apply(vec![("userid", marshal::Value::Int(account)), ("charid", marshal::Value::Int(other))]);
        assert!(matches!(super::dispatch(&mut owner_client, &uninsure).await.unwrap(), Ok(marshal::Value::None)));
        assert!(setup.query_opt("SELECT ship_id FROM game.insurance WHERE ship_id=$1", &[&ship]).await.unwrap().is_none(), "the owner removes that policy");
        assert!(matches!(super::dispatch(&mut owner_client, &uninsure).await.unwrap(), Ok(marshal::Value::None)));
        setup.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
        assert!(matches!(super::dispatch(&mut owner_client, &uninsure).await.unwrap(), Ok(marshal::Value::None)), "the deleted fixture hull is a missing hull");
        let final_balance: String = setup.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
        assert_eq!(final_balance, before_balance, "cancellation leaves the original balance");
    }).await;
    // Cleanup runs on its own connection so a timeout or panic in the race
    // still removes the fixture rows.
    let (cleanup, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let owners = [owner, other];
    cleanup.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1)", &[&&owners[..]]).await.unwrap();
    cleanup.execute("DELETE FROM game.insurance WHERE ship_id=$1", &[&ship]).await.unwrap();
    cleanup.execute("DELETE FROM game.items WHERE owner_id=ANY($1)", &[&&owners[..]]).await.unwrap();
    cleanup.execute("DELETE FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap();
    cleanup.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    raced.unwrap();
}
