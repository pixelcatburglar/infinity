//! Test-only cleanup for isolated mailing-list fixtures.
use crate::db::{Db, Error};

pub(crate) async fn cleanup(db: &Db, owner: i64, characters: &[i64]) -> Result<(), Error> {
    let lists: Vec<i64> = db.query("SELECT list_id FROM game.mailing_lists WHERE owner_id = $1", &[&owner]).await?.iter().map(|r| r.get("list_id")).collect();
    let messages: Vec<i64> = db.query("SELECT message_id FROM game.mail_messages WHERE to_list_id = ANY($1) OR sender_id = ANY($2)", &[&lists, &characters]).await?.iter().map(|r| r.get("message_id")).collect();
    db.execute("DELETE FROM game.mail_status WHERE message_id = ANY($1)", &[&messages]).await?;
    db.execute("DELETE FROM game.mail_messages WHERE message_id = ANY($1)", &[&messages]).await?;
    db.execute("DELETE FROM game.mailing_list_access WHERE list_id = ANY($1)", &[&lists]).await?;
    db.execute("DELETE FROM game.mailing_list_members WHERE list_id = ANY($1)", &[&lists]).await?;
    db.execute("DELETE FROM game.mailing_lists WHERE list_id = ANY($1)", &[&lists]).await?;
    Ok(())
}

/// Reject only new messages for a fixture-owned list; existing rows are not validated.
pub(crate) async fn reject_list_messages(db: &Db, list: i64) -> Result<(), Error> {
    let sql = format!("ALTER TABLE game.mail_messages ADD CONSTRAINT \"mailing_list_fixture_reject_{list}\" CHECK (to_list_id IS DISTINCT FROM {list}) NOT VALID");
    db.execute(&sql, &[]).await?;
    Ok(())
}

pub(crate) async fn allow_list_messages(db: &Db, list: i64) -> Result<(), Error> {
    let sql = format!("ALTER TABLE game.mail_messages DROP CONSTRAINT IF EXISTS \"mailing_list_fixture_reject_{list}\"");
    db.execute(&sql, &[]).await?;
    Ok(())
}

pub(crate) async fn allow_owned_list_messages(db: &Db, owner: i64) -> Result<(), Error> {
    for row in db.query("SELECT list_id FROM game.mailing_lists WHERE owner_id = $1", &[&owner]).await? {
        allow_list_messages(db, row.get("list_id")).await?;
    }
    Ok(())
}

/// Exact, ordered JSON rows for one list and all messages addressed to it.
pub(crate) async fn snapshot(db: &Db, list: i64) -> Result<Vec<Vec<String>>, Error> {
    let queries = [
        "SELECT row_to_json(t)::text AS row FROM game.mailing_lists t WHERE list_id = $1 ORDER BY list_id",
        "SELECT row_to_json(t)::text AS row FROM game.mailing_list_members t WHERE list_id = $1 ORDER BY character_id",
        "SELECT row_to_json(t)::text AS row FROM game.mailing_list_access t WHERE list_id = $1 ORDER BY entity_id",
        "SELECT row_to_json(t)::text AS row FROM game.mail_messages t WHERE to_list_id = $1 ORDER BY message_id",
        "SELECT row_to_json(s)::text AS row FROM game.mail_status s JOIN game.mail_messages m ON m.message_id = s.message_id WHERE m.to_list_id = $1 ORDER BY s.message_id, s.char_id",
    ];
    let mut result = Vec::new();
    for sql in queries {
        result.push(db.query(sql, &[&list]).await?.iter().map(|r| r.get("row")).collect());
    }
    Ok(result)
}

pub(crate) async fn dummy_characters(db: &Db, account: i64, count: i64) -> Result<Vec<i64>, Error> {
    let rows = db.query("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) SELECT $1, 'mail-capacity-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1 FROM generate_series(1::bigint, $2::bigint) RETURNING character_id", &[&account, &count]).await?;
    Ok(rows.iter().map(|r| r.get("character_id")).collect())
}

pub(crate) async fn seed_members(db: &Db, list: i64, characters: &[i64]) -> Result<(), Error> {
    db.execute("INSERT INTO game.mailing_list_members (list_id, character_id, member_access, joined_at) SELECT $1, id, 1, 0 FROM unnest($2::bigint[]) AS id ORDER BY id", &[&list, &characters]).await?;
    Ok(())
}

pub(crate) async fn delete_dummy_characters(db: &Db, characters: &[i64]) -> Result<(), Error> {
    db.execute("DELETE FROM game.characters WHERE character_id = ANY($1)", &[&characters]).await?;
    Ok(())
}

pub(crate) async fn organizations(db: &Db, creator: i64) -> Result<(i64, i64, i64), Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let first: i64 = tx.query_one("INSERT INTO game.corporations (name, ticker, creator_id, ceo_id, station_id, created) VALUES ('mail corp ' || ($1::bigint)::text, 'MC' || ($1::bigint)::text, $1, $1, 1, 0) RETURNING corporation_id", &[&creator]).await?.get("corporation_id");
        let second: i64 = tx.query_one("INSERT INTO game.corporations (name, ticker, creator_id, ceo_id, station_id, created) VALUES ('mail other ' || ($1::bigint)::text, 'MO' || ($1::bigint)::text, $1, $1, 1, 0) RETURNING corporation_id", &[&creator]).await?.get("corporation_id");
        let alliance: i64 = tx.query_one("INSERT INTO game.alliances (name, short_name, executor_corp_id, creator_corp_id, creator_char_id, created) VALUES ('mail alliance ' || ($1::bigint)::text, 'MA' || ($1::bigint)::text, $2::bigint, $2, $1, 0) RETURNING alliance_id", &[&creator, &first]).await?.get("alliance_id");
        tx.execute("UPDATE game.corporations SET alliance_id = $2 WHERE corporation_id = $1", &[&first, &alliance]).await?;
        Ok((first, second, alliance))
    }), |value, ()| async move { Ok(value) }).await
}

pub(crate) async fn set_corporation(db: &Db, character: i64, corporation: i64) -> Result<(), Error> {
    db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&character, &corporation]).await?;
    Ok(())
}

pub(crate) async fn set_deleted(db: &Db, character: i64, deleted: bool) -> Result<(), Error> {
    db.execute("UPDATE game.characters SET deleted = $2 WHERE character_id = $1", &[&character, &deleted]).await?;
    Ok(())
}

pub(crate) async fn delete_organizations(db: &Db, first: i64, second: i64, alliance: i64) -> Result<(), Error> {
    db.execute("DELETE FROM game.alliances WHERE alliance_id = $1", &[&alliance]).await?;
    db.execute("DELETE FROM game.corporations WHERE corporation_id = ANY($1)", &[&vec![first, second]]).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn normalization_uses_postgresql_case_mapping_and_python_whitespace() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (key, display) = super::normalize_name(&db, "前缀\\ A  B\t\t中\u{00a0}\u{001c}文").await.unwrap();
    assert_eq!(key, "ab中文");
    assert_eq!(display, "前缀\\ A B\t中\u{00a0}\u{001c}文");
    for value in ["ß", "İ", "ASCII", "中文"] {
        let (key, _) = super::normalize_name(&db, value).await.unwrap();
        let row = db.query_one("SELECT lower(upper($1::text)) AS folded", &[&value]).await.unwrap();
        let first: String = row.get("folded");
        let (expected, _) = super::normalize_name(&db, &first).await.unwrap();
        assert_eq!(key, expected);
    }
}
