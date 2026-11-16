//! Real GAME_SCHEMA replay coverage for the capped player-corporation ID sequence.

use super::*;
use infinity_core::consts::{MAX_PLAYER_CORPORATION_ID as MAX, MIN_PLAYER_CORPORATION_ID as MIN};

const FIXTURE_NAME: &str = "corporation-ids-boundary-fixture";

/// (last_value, is_called, MAXVALUE, CYCLE) as one before/after comparison tuple.
async fn sequence_state(client: &tokio_postgres::Client) -> (i64, bool, i64, bool) {
    let row = client.query_one("SELECT s.last_value, s.is_called, q.seqmax, q.seqcycle FROM game.corporations_corporation_id_seq AS s JOIN pg_sequence AS q ON q.seqrelid = 'game.corporations_corporation_id_seq'::regclass", &[]).await.unwrap();
    (row.get("last_value"), row.get("is_called"), row.get("seqmax"), row.get("seqcycle"))
}

async fn set_value(client: &tokio_postgres::Client, value: i64, called: bool) {
    client.query_one("SELECT setval('game.corporations_corporation_id_seq', $1, $2) AS value", &[&value, &called]).await.unwrap();
}

/// Boundary fixture row; `deleted` keeps it out of the unique name/ticker indexes.
async fn insert_row(client: &tokio_postgres::Client, id: i64) {
    client.execute("INSERT INTO game.corporations (corporation_id, name, ticker, creator_id, station_id, created, deleted) VALUES ($1, $2, 'CIDSCAP', 1, 1, 0, true)", &[&id, &FIXTURE_NAME]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with external flock; run this test alone"]
async fn corporation_id_cap_replays_and_rejects_out_of_range_state() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let client = db.pool.get().await.unwrap();
    // Captured before the first sequence mutation for unconditional restoration.
    let saved = sequence_state(&client).await;
    let result = crate::tests::catch_scenario(async {
        // Fresh initialization on a transaction-local schema clone.
        client.batch_execute("BEGIN; DROP SCHEMA game CASCADE;").await.unwrap();
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        assert_eq!(sequence_state(&client).await, (MIN, true, MAX, false));
        let first: i64 = client.query_one("SELECT nextval('game.corporations_corporation_id_seq') AS id", &[]).await.unwrap().get("id");
        assert_eq!(first, MIN + 1);
        // Repeated in-range replay is a no-op while the sequence leads.
        set_value(&client, MIN + 100, true).await;
        for _ in 0..2 { client.batch_execute(GAME_SCHEMA).await.unwrap(); }
        assert_eq!(sequence_state(&client).await, (MIN + 100, true, MAX, false));
        // A row high-water mark, counting deleted rows, only advances it.
        insert_row(&client, MIN + 200).await;
        set_value(&client, MIN + 50, true).await;
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        assert_eq!(sequence_state(&client).await, (MIN + 200, true, MAX, false));
        set_value(&client, MIN + 300, true).await;
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        assert_eq!(sequence_state(&client).await, (MIN + 300, true, MAX, false));
        // An exhausted but valid sequence still replays.
        set_value(&client, MAX, true).await;
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        assert_eq!(sequence_state(&client).await, (MAX, true, MAX, false));
        client.batch_execute("ROLLBACK").await.unwrap();
        // Legacy last_value above the cap: the boundary ALTER rejects it
        // (PostgreSQL checks the stored last_value regardless of is_called).
        client.batch_execute("BEGIN; DROP SCHEMA game CASCADE;").await.unwrap();
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        client.batch_execute(&format!("ALTER SEQUENCE game.corporations_corporation_id_seq MAXVALUE {}", MAX + 2)).await.unwrap();
        set_value(&client, MAX + 1, false).await;
        let staged = sequence_state(&client).await;
        client.batch_execute("SAVEPOINT before_replay").await.unwrap();
        let error = client.batch_execute(GAME_SCHEMA).await.unwrap_err();
        assert!(error.as_db_error().unwrap().message().contains("MAXVALUE"), "{error}");
        client.batch_execute("ROLLBACK TO SAVEPOINT before_replay").await.unwrap();
        assert_eq!(sequence_state(&client).await, staged);
        client.batch_execute("ROLLBACK").await.unwrap();
        // A legacy deleted row above the cap: the high-water setval rejects
        // it and the rolled-back ALTER restores the raised bound, leaving
        // sequence and row intact.
        client.batch_execute("BEGIN; DROP SCHEMA game CASCADE;").await.unwrap();
        client.batch_execute(GAME_SCHEMA).await.unwrap();
        client.batch_execute(&format!("ALTER SEQUENCE game.corporations_corporation_id_seq MAXVALUE {}", MAX + 2)).await.unwrap();
        insert_row(&client, MAX + 1).await;
        set_value(&client, MIN + 1, false).await;
        let staged = sequence_state(&client).await;
        client.batch_execute("SAVEPOINT before_replay").await.unwrap();
        let error = client.batch_execute(GAME_SCHEMA).await.unwrap_err();
        assert!(error.as_db_error().unwrap().message().contains("corporations_corporation_id_seq"), "{error}");
        client.batch_execute("ROLLBACK TO SAVEPOINT before_replay").await.unwrap();
        assert_eq!(sequence_state(&client).await, staged);
        assert!(client.query_opt("SELECT deleted FROM game.corporations WHERE corporation_id = $1", &[&(MAX + 1)]).await.unwrap().is_some());
        client.batch_execute("ROLLBACK").await.unwrap();
    }).await;
    let cleanup = crate::tests::catch_scenario(async {
        client.batch_execute("ROLLBACK").await.unwrap();
        client.execute("DELETE FROM game.corporations WHERE name = $1", &[&FIXTURE_NAME]).await.unwrap();
    }).await;
    // Unconditional restoration: last_value and is_called before the bounds,
    // because a staged out-of-range last_value would reject a lower MAXVALUE.
    let value = client.query_one("SELECT setval('game.corporations_corporation_id_seq', $1, $2) AS value", &[&saved.0, &saved.1]).await;
    let bounds = client.batch_execute(&format!("ALTER SEQUENCE game.corporations_corporation_id_seq MAXVALUE {} {}", saved.2, if saved.3 { "CYCLE" } else { "NO CYCLE" })).await;
    value.expect("restoring corporation sequence last_value/is_called");
    bounds.expect("restoring corporation sequence bounds");
    assert_eq!(sequence_state(&client).await, saved, "corporation sequence state was not restored");
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
}
