//! Transactional regressions for scoped asteroid depletion.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn scoped_depletion_preserves_ownership_and_atomically_marks_last_rock() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let system: i64 = tx.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let field = system;
    let other: i64 = tx.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    assert!(grown_batch(&tx, system, &[field], 10).await.unwrap());
    assert!(!grown_batch(&tx, other, &[field], 11).await.unwrap());
    assert_eq!(self::field(&tx, field).await.unwrap().unwrap().system, system);
    let rocks = insert_batch(&tx, system, &[
        NewAsteroid { field, type_id: 1, quantity: 0, radius: 1.0, pos: [0.0; 3] },
        NewAsteroid { field, type_id: 1, quantity: 3, radius: 1.0, pos: [0.0; 3] },
    ]).await.unwrap();
    let zero = rocks[0].id;
    let positive = rocks[1].id;
    for (owner, candidate_field, id) in [(other, field, zero), (system, other, zero), (system, field, positive)] {
        assert_eq!(deplete_scoped(&tx, owner, candidate_field, id, 20, Some(10)).await.unwrap(), Depletion::AbsentWrongField);
    }
    let save = tx.savepoint("depletion_rollback").await.unwrap();
    assert_eq!(deplete_scoped(&save, system, field, zero, 20, Some(10)).await.unwrap(), Depletion::Deleted { field, deadline: None });
    save.rollback().await.unwrap();
    assert_eq!(load(&tx, system).await.unwrap().len(), 2);
    assert_eq!(deplete_scoped(&tx, system, field, zero, 20, Some(10)).await.unwrap(), Depletion::Deleted { field, deadline: None });
    assert_eq!(deplete_scoped(&tx, system, field, zero, 21, Some(10)).await.unwrap(), Depletion::AlreadyDepleted { field, deadline: None });
    assert_eq!(self::field(&tx, field).await.unwrap().unwrap().depleted_at, None);
    assert!(take(&tx, positive, 3, 3).await.unwrap());
    let save = tx.savepoint("last_rollback").await.unwrap();
    assert_eq!(deplete_scoped(&save, system, field, positive, 30, Some(10)).await.unwrap(), Depletion::Deleted { field, deadline: Some(40) });
    assert_eq!(self::field(&save, field).await.unwrap().unwrap().depleted_at, Some(30));
    save.rollback().await.unwrap();
    assert_eq!(load(&tx, system).await.unwrap().len(), 1);
    assert_eq!(self::field(&tx, field).await.unwrap().unwrap().depleted_at, None);
    assert_eq!(deplete_scoped(&tx, system, field, positive, 30, Some(10)).await.unwrap(), Depletion::Deleted { field, deadline: Some(40) });
    assert_eq!(deplete_scoped(&tx, system, field, positive, 50, Some(10)).await.unwrap(), Depletion::AlreadyDepleted { field, deadline: Some(40) });
    assert_eq!(deplete_scoped(&tx, other, field, positive, 50, Some(10)).await.unwrap(), Depletion::AbsentWrongField);
    assert_eq!(self::field(&tx, field).await.unwrap().unwrap().depleted_at, Some(30));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn committed_batch_preserves_each_input_to_returned_id_binding() {
    let cfg = crate::config::tests::test_database();
    let (control, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let system: i64 = control.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let result = tokio::spawn(async move {
        let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = client.transaction().await.unwrap();
        let inputs = [
            NewAsteroid { field: system, type_id: 7, quantity: 13, radius: 2.0, pos: [1.0, 2.0, 3.0] },
            NewAsteroid { field: system, type_id: 8, quantity: 29, radius: 4.0, pos: [-1.0, -2.0, -3.0] },
        ];
        let inserted = insert_batch(&tx, system, &inputs).await.unwrap();
        assert_eq!(inserted.len(), inputs.len());
        assert_ne!(inserted[0].id, inserted[1].id);
        tx.commit().await.unwrap();
        for (input, rock) in inputs.iter().zip(&inserted) {
            assert_eq!((rock.field, rock.type_id, rock.quantity, rock.radius, [rock.x, rock.y, rock.z]),
                (input.field, input.type_id, input.quantity, Some(input.radius), input.pos));
            let row = client.query_one(
                "SELECT system_id, field_id, type_id, quantity, original_quantity, radius, x, y, z FROM game.asteroids WHERE asteroid_id=$1",
                &[&rock.id],
            ).await.unwrap();
            assert_eq!(row.get::<_, i64>("system_id"), system);
            assert_eq!(row.get::<_, i64>("field_id"), input.field);
            assert_eq!(row.get::<_, i32>("type_id"), input.type_id);
            assert_eq!(row.get::<_, i64>("quantity"), input.quantity);
            assert_eq!(row.get::<_, i64>("original_quantity"), input.quantity);
            assert_eq!(row.get::<_, Option<f32>>("radius"), Some(input.radius));
            assert_eq!([row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")], input.pos);
        }
    }).await;
    control.execute("DELETE FROM game.asteroids WHERE system_id=$1", &[&system]).await.unwrap();
    result.unwrap();
}
