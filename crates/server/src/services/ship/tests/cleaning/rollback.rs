//! Failure injection and persistence snapshots for ship cleaning RPC tests.
//! The client cleaning request reaches game items and ship_dirt through dispatch.
//! A fixture-scoped constraint rejects persistence; no real client UI is exercised.
use super::*;

pub(super) async fn fingerprint(db: &crate::db::Db, ship: i64) -> String {
    db.query_one("SELECT row_to_json(i)::text || COALESCE((SELECT string_agg(row_to_json(a)::text, '' ORDER BY a.attribute_id) FROM game.item_attributes a WHERE a.item_id=i.item_id),'') || COALESCE((SELECT row_to_json(d)::text FROM game.ship_dirt d WHERE d.ship_item_id=i.item_id),'') AS fingerprint FROM game.items i WHERE i.item_id=$1", &[&ship]).await.unwrap().get("fingerprint")
}

pub(super) async fn failed_reset(c: &mut Client, db: &crate::db::Db, ship: i64, owner: i64, bind: &Value) {
    let before = fingerprint(db, ship).await;
    let session: Vec<_> = c.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
    let notices = c.outbox.len();
    let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
    c.notify_tx = notify;
    let constraint = format!("ship_dirt_reject_{owner}");
    db.execute(&format!("ALTER TABLE game.ship_dirt ADD CONSTRAINT {constraint} CHECK (ship_item_id <> {ship}) NOT VALID"), &[]).await.unwrap();
    let result = rpc(c, "ship", "ResetDirtTimestamp", &[Value::Int(ship)], bind).await;
    db.execute(&format!("ALTER TABLE game.ship_dirt DROP CONSTRAINT {constraint}"), &[]).await.unwrap();
    assert!(matches!(result, Err(CallError::Internal(_))));
    assert_eq!(fingerprint(db, ship).await, before);
    assert_eq!(c.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
    assert_eq!(c.outbox.len(), notices);
    assert!(events.try_recv().is_err());
}
