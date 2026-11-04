//! Dedicated database coverage of persistent ship SKIN state and guarded mutations.
//! The client-facing cosmetics services own authorization and compatibility; these tests
//! exercise only `game` rows and do not cover service or gateway behavior.
use super::*;
mod lifecycle;
mod expiry;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn licenses_applications_guards_and_cascades() {
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    db.transaction::<(), (), Error, (), _, _, _>(|tx, _| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('skin-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'skin-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let item_id: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,1,1,1,true) RETURNING item_id", &[&character]).await?.get("item_id");
        let ship = Item { item_id, type_id: 1, owner_id: character, location_id: 1, flag_id: 1, quantity: 1, singleton: true, group_id: 1, category_id: 1, custom_info: None };
        let now = 100_i64;
        assert!(insert_license(tx, character, 1, None).await?);
        assert!(!insert_license(tx, character, 1, Some(now)).await?);
        assert_eq!(get_license(tx, character, 1, now).await?.unwrap().expires_at, None);
        assert!(insert_license(tx, character, 2, Some(now)).await?);
        assert!(insert_license(tx, character, 3, Some(now - 1)).await?);
        let licenses = licensed(tx, character, now).await?;
        assert_eq!(licenses.iter().map(|l| l.skin_id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(get_license(tx, character, 2, now).await?.unwrap().expires_at, Some(now));
        assert!(get_license(tx, character, 3, now).await?.is_none());
        assert!(insert_license(tx, character, 4, Some(now - 1)).await?);
        assert_eq!(get_stored_license(tx, character, 4).await?.unwrap().expires_at, Some(now - 1));
        assert!(!update_expiry(tx, character, 4, Some(now), None).await?);
        assert!(update_expiry(tx, character, 4, Some(now - 1), None).await?);
        assert_eq!(get_license(tx, character, 4, now).await?.unwrap().expires_at, None);
        assert!(get_license(tx, character, 3, now).await?.is_none());
        assert!(set_application(tx, character, &ship, 2, now).await?);
        let states = states_for_ships(tx, &[item_id], now).await?;
        assert_eq!(states.len(), 1);
        assert_eq!((states[0].ship_item_id, states[0].type_id, states[0].owner_id), (item_id, 1, character));
        assert_eq!((states[0].character_id, states[0].skin_id), (Some(character), Some(2)));
        assert_eq!(states_for_ships(tx, &[item_id], now + 1).await?[0].skin_id, None);
        assert!(!set_application(tx, character, &ship, 3, now).await?);
        let mut stale = ship.clone();
        stale.location_id += 1;
        assert!(!set_application(tx, character, &stale, 1, now).await?);
        assert!(!clear_application(tx, character, &stale).await?);
        assert_eq!(states_for_ships(tx, &[item_id], now).await?[0].skin_id, Some(2));
        tx.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&item_id, &(character + 1)]).await?;
        let state = states_for_ships(tx, &[item_id], now).await?;
        assert_eq!((state[0].character_id, state[0].skin_id), (None, None));
        assert!(!set_application(tx, character, &ship, 1, now).await?);
        tx.execute("UPDATE game.items SET owner_id=$2, singleton=false WHERE item_id=$1", &[&item_id, &character]).await?;
        assert_eq!(states_for_ships(tx, &[item_id], now).await?[0].skin_id, None);
        assert!(!clear_application(tx, character, &ship).await?);
        tx.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&item_id]).await?;
        let other: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'skin-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        assert!(insert_license(tx, other, 1, None).await?);
        tx.execute("UPDATE game.ship_skin_applications SET character_id=$2, skin_id=1 WHERE ship_item_id=$1", &[&item_id, &other]).await?;
        assert!(clear_application(tx, character, &ship).await?);
        let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.ship_skin_applications WHERE ship_item_id=$1", &[&item_id]).await?.get("count");
        assert_eq!(count, 0);
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&other]).await?;
        assert!(clear_application(tx, character, &ship).await?);
        assert!(clear_application(tx, character, &ship).await?);
        assert_eq!(states_for_ships(tx, &[item_id], now).await?[0].skin_id, None);
        assert!(set_application(tx, character, &ship, 1, now).await?);
        tx.execute("DELETE FROM game.ship_skin_licenses WHERE character_id=$1 AND skin_id=1", &[&character]).await?;
        assert_eq!(states_for_ships(tx, &[item_id], now).await?[0].skin_id, None);
        let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.ship_skin_applications WHERE ship_item_id=$1", &[&item_id]).await?.get("count");
        assert_eq!(count, 0);
        assert!(set_application(tx, character, &ship, 2, now).await?);
        tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&item_id]).await?;
        let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.ship_skin_applications WHERE ship_item_id=$1", &[&item_id]).await?.get("count");
        assert_eq!(count, 0);
        assert!(get_license(tx, character, 2, now).await?.is_some());
        tx.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&character]).await?;
        assert!(licensed(tx, character, now).await?.is_empty());
        assert!(get_license(tx, character, 2, now).await?.is_none());
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
}
