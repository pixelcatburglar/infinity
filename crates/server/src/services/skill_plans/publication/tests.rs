//! Publication gate serialization, release and committed audiences.
use super::*;
use crate::gateway::notices::NoticeHub;
use crate::services::tests::fixture;
use std::time::Duration;

#[tokio::test]
async fn gate_serializes_one_corporation_and_releases_others() {
    let hub = NoticeHub::new();
    let first = hub.corporation_publication(10_000_001).await;
    assert!(tokio::time::timeout(Duration::from_millis(50), hub.corporation_publication(10_000_001)).await.is_err());
    let other = hub.corporation_publication(10_000_002).await;
    drop(other);
    drop(first);
    assert!(tokio::time::timeout(Duration::from_secs(2), hub.corporation_publication(10_000_001)).await.is_ok());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn audience_is_committed_membership_and_publish_queues_the_notice() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let first = fixture::create(&db).await.unwrap();
    let second = fixture::create(&db).await.unwrap();
    let departed = fixture::create(&db).await.unwrap();
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('aud-' || nextval('game.corporations_corporation_id_seq')::text, 'AU' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&first.character]).await.unwrap().get("corporation_id");
    for member in [first, second, departed] { db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&member.character, &corp]).await.unwrap(); }
    db.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&departed.character]).await.unwrap();
    let mut expected = vec![first.character, second.character];
    expected.sort_unstable();
    assert_eq!(&*audience(&db, corp).await.unwrap().members, expected.as_slice());
    let payload = prost_reflect::DynamicMessage::new(crate::gateway::message::message("eve_public.Event").unwrap());
    let mut outbox = Outbox::default();
    let notice = move |audience: Audience| {
        assert_eq!(audience.corporation, corp);
        assert_eq!(&*audience.members, expected.as_slice());
        GatewayNotice::new(crate::gateway::protocol::target_group::Target::Corporation(u32::try_from(audience.corporation).unwrap()), &payload).map_err(crate::gateway::notices::producer_error)
    };
    publish(&db, &mut outbox, corp, &notice).await.unwrap();
    let entries = outbox.into_entries();
    assert!(matches!(entries.as_slice(), [crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Gateway(_))]));
    db.execute("UPDATE game.characters SET corporation_id=1, deleted=false WHERE character_id=ANY($1)", &[&vec![first.character, second.character, departed.character]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for member in [first, second, departed] { fixture::cleanup(&db, member).await.unwrap(); }
}
