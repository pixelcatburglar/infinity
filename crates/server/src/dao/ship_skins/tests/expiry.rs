//! Dedicated database coverage for timed license removal and scheduling queries.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn expiry_queries_and_guarded_removal() {
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    db.transaction::<(), (), Error, (), _, _, _>(|tx, _| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('skin-expiry-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'skin-expiry-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let other: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'skin-expiry-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let mut ships = Vec::new();
        for (location, type_id) in [(11_i64, 1_i32), (12, 2)] {
            let item_id = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,1,1,true) RETURNING item_id", &[&type_id, &character, &location]).await?.get("item_id");
            ships.push(Item { item_id, type_id, owner_id: character, location_id: location, flag_id: 1, quantity: 1, singleton: true, group_id: 1, category_id: 1, custom_info: None });
        }
        assert!(insert_license(tx, character, 1, Some(100)).await?);
        assert!(insert_license(tx, character, 2, Some(200)).await?);
        assert!(insert_license(tx, character, 3, None).await?);
        assert!(insert_license(tx, other, 1, Some(50)).await?);
        assert_eq!(next_expiry(tx, character).await?, Some(100));
        let finite = finite_characters(tx).await?;
        assert!(finite.binary_search(&character).is_ok());
        assert!(finite.binary_search(&other).is_ok());
        assert!(due_licenses(tx, character, 100).await?.is_empty());
        assert!(remove_license(tx, character, 1, Some(100)).await?.is_none());
        for ship in &ships { assert!(set_application(tx, character, ship, 1, 100).await?); }
        let applications = applications_for_character(tx, character).await?;
        assert_eq!(applications.iter().map(|a| (a.ship_item_id, a.type_id, a.location_id)).collect::<Vec<_>>(), ships.iter().map(|s| (s.item_id, s.type_id, s.location_id)).collect::<Vec<_>>());
        assert_eq!(due_licenses(tx, character, 101).await?.iter().map(|l| l.skin_id).collect::<Vec<_>>(), vec![1]);
        let removed = remove_license(tx, character, 1, Some(101)).await?.unwrap();
        assert_eq!(removed.skin_id, 1);
        assert_eq!(removed.applications.iter().map(|a| (a.ship_item_id, a.type_id, a.location_id)).collect::<Vec<_>>(), ships.iter().map(|s| (s.item_id, s.type_id, s.location_id)).collect::<Vec<_>>());
        assert!(applications_for_character(tx, character).await?.is_empty());
        assert!(remove_license(tx, character, 1, None).await?.is_none());
        assert_eq!(next_expiry(tx, character).await?, Some(200));
        assert!(update_expiry(tx, character, 2, Some(200), Some(300)).await?);
        assert!(remove_license(tx, character, 2, Some(201)).await?.is_none());
        assert_eq!(next_expiry(tx, character).await?, Some(300));
        let zero = remove_license(tx, character, 2, Some(301)).await?.unwrap();
        assert_eq!(zero.skin_id, 2);
        assert!(zero.applications.is_empty());
        assert_eq!(next_expiry(tx, character).await?, None);
        assert!(finite_characters(tx).await?.binary_search(&character).is_err());
        let permanent = remove_license(tx, character, 3, None).await?.unwrap();
        assert_eq!(permanent.skin_id, 3);
        assert!(permanent.applications.is_empty());
        tx.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&other]).await?;
        assert!(finite_characters(tx).await?.binary_search(&other).is_err());
        assert_eq!(next_expiry(tx, other).await?, None);
        assert!(due_licenses(tx, other, 100).await?.is_empty());
        for ship in &ships { tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship.item_id]).await?; }
        tx.execute("DELETE FROM game.characters WHERE character_id=ANY($1)", &[&&[character, other][..]]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
}
