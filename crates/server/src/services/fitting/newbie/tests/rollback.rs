use super::*;
use infinity_core::consts::TYPE_CAPSULE;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn insert_failure_rolls_back_both_grants_and_retry_succeeds() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    for method in ["CreateNewbieBonusItems", "CreateNewbieShip"] {
        let (f, station, system, old) = setup(&db, &sde, race, TYPE_CAPSULE).await;
        let function = format!("grant_failure_{}", f.character);
        let trigger = format!("grant_failure_{}", f.character);
        let sql = format!("CREATE FUNCTION game.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.owner_id = {} AND (SELECT count(*) FROM game.items WHERE owner_id = {}) >= 3 THEN RAISE EXCEPTION 'fixture failure'; END IF; RETURN NEW; END $$", f.character, f.character);
        db.execute(&sql, &[]).await.unwrap();
        db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.items FOR EACH ROW EXECUTE FUNCTION game.{function}()"), &[]).await.unwrap();
        let test_db = db.clone();
        let test_sde = sde.clone();
        let result = tokio::spawn(async move {
            let mut c = client(test_db.clone(), test_sde, f, station, system, old).await;
            let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
            c.notify_tx = sender.clone();
            c.world.register_pilot(f.character, sender);
            c.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&c.session));
            let before = snapshot(&test_db, f.character).await;
            let args = if method == "CreateNewbieShip" { vec![Value::Int(old), Value::Int(station)] } else { vec![Value::Int(station)] };
            let queued = c.outbox.len();
            assert!(matches!(rpc(&mut c, method, &args, &Value::None).await, Err(crate::services::CallError::Internal(_))));
            assert_eq!(snapshot(&test_db, f.character).await, before);
            assert_eq!(c.session.int("shipid"), Some(old));
            assert_eq!(c.outbox.len(), queued);
            assert!(received.try_recv().is_err());
            test_db.execute(&format!("DROP TRIGGER {trigger} ON game.items"), &[]).await.unwrap();
            test_db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await.unwrap();
            assert_eq!(rpc(&mut c, method, &args, &Value::None).await.unwrap(), Value::None);
        }).await;
        db.execute(&format!("DROP TRIGGER IF EXISTS grant_failure_{} ON game.items", f.character), &[]).await.unwrap();
        db.execute(&format!("DROP FUNCTION IF EXISTS game.grant_failure_{}()", f.character), &[]).await.unwrap();
        cleanup(&db, f).await;
        result.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn corrupted_fitted_batch_rolls_back_before_boarding() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    let (f, station, system, old) = setup(&db, &sde, race, TYPE_CAPSULE).await;
    let function = format!("grant_corruption_{}", f.character);
    let trigger = function.clone();
    db.execute(&format!("CREATE FUNCTION game.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.owner_id = {} AND NEW.location_id <> {} THEN NEW.quantity := NEW.quantity + 1; END IF; RETURN NEW; END $$", f.character, station), &[]).await.unwrap();
    db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.items FOR EACH ROW EXECUTE FUNCTION game.{function}()"), &[]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut c = client(test_db.clone(), sde, f, station, system, old).await;
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender.clone();
        c.world.register_pilot(f.character, sender);
        c.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&c.session));
        let before = snapshot(&test_db, f.character).await;
        let args = [Value::Int(old), Value::Int(station)];
        assert!(matches!(rpc(&mut c, "CreateNewbieShip", &args, &Value::None).await, Err(crate::services::CallError::Internal(_))));
        assert_eq!(snapshot(&test_db, f.character).await, before);
        assert_eq!(c.session.int("shipid"), Some(old));
        assert!(c.outbox.is_empty());
        assert!(received.try_recv().is_err());
        test_db.execute(&format!("DROP TRIGGER {trigger} ON game.items"), &[]).await.unwrap();
        test_db.execute(&format!("DROP FUNCTION game.{function}()"), &[]).await.unwrap();
        assert_eq!(rpc(&mut c, "CreateNewbieShip", &args, &Value::None).await.unwrap(), Value::None);
    }).await;
    db.execute(&format!("DROP TRIGGER IF EXISTS grant_corruption_{} ON game.items", f.character), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION IF EXISTS game.grant_corruption_{}()", f.character), &[]).await.unwrap();
    cleanup(&db, f).await;
    result.unwrap();
}
