//! Immutable war projection and rollback on a dedicated database.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn projection_is_unique_and_preserves_sides() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let save = tx.savepoint("projection_fixture").await.unwrap();
    let a: i64 = save.query_one("SELECT nextval('game.wars_war_id_seq') AS id", &[]).await.unwrap().get("id");
    let b = -a;
    let war = crate::dao::wars::create(&save, a, b, 1, 2).await.unwrap().unwrap();
    let mut new = crate::dao::killmails::NewKillmail {
        victim_item_id: None,
        now: 3, system: 1, victim: 1, victim_corp: 1, victim_alliance: Some(10),
        ship_type: 1, damage_taken: 0.0, final_char: None, final_corp: None,
        final_alliance: Some(20), final_ship_type: 1, final_weapon_type: 1,
        final_damage: 0.0, final_sec: 0.0, isk_lost: 0.0, involved: &[], blob: "",
    };
    let kill_id = crate::dao::killmails::insert(&save, &new).await.unwrap();
    new.victim_alliance = None;
    new.final_alliance = None;
    let null_kill_id = crate::dao::killmails::insert(&save, &new).await.unwrap();
    let invalid = Projection { kill_id, war_id: war.war_id, attacker_side: b, defender_side: 0 };
    assert!(!insert(&save, &invalid).await.unwrap());
    assert!(!insert(&save, &Projection { defender_side: b, ..invalid }).await.unwrap());
    assert!(by_kills(&save, &[kill_id]).await.unwrap().is_empty());
    let p = Projection { kill_id, war_id: war.war_id, attacker_side: b, defender_side: a };
    assert!(insert(&save, &p).await.unwrap());
    let reversed = Projection { attacker_side: a, defender_side: b, ..p.clone() };
    assert!(!insert(&save, &reversed).await.unwrap());
    assert!(insert(&save, &Projection { kill_id: null_kill_id, ..p.clone() }).await.unwrap());
    let rows = killmails(&save, war.war_id).await.unwrap();
    assert_eq!(rows.len(), 2);
    let first = rows.iter().find(|row| row.projection.kill_id == kill_id).unwrap();
    assert_eq!((first.projection.attacker_side, first.projection.defender_side), (b, a));
    assert_eq!((first.killmail.victim_alliance_id, first.killmail.final_alliance_id), (Some(10), Some(20)));
    let null = rows.iter().find(|row| row.projection.kill_id == null_kill_id).unwrap();
    assert_eq!((null.killmail.victim_alliance_id, null.killmail.final_alliance_id), (None, None));
    assert_eq!(by_kills(&save, &[kill_id]).await.unwrap().len(), 1);
    assert_eq!(killmails(&save, war.war_id).await.unwrap().len(), 2);
    save.rollback().await.unwrap();
    assert!(by_kills(&tx, &[kill_id]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
}
