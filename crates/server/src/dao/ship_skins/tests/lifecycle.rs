//! Dedicated database regression coverage for item-root SKIN cleanup.
use super::*;
use crate::consts::FLAG_HANGAR;
use crate::dao::{exploration, items::{fitting, inventory, transfers}};
use crate::db::executor::Executor;

async fn item(ex: &(impl Executor + ?Sized), owner: i64, location: i64) -> Result<Item, Error> {
    let item_id = ex.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,$2,$3,1,true) RETURNING item_id", &[&owner, &location, &FLAG_HANGAR]).await?.get("item_id");
    Ok(Item { item_id, type_id: 1, owner_id: owner, location_id: location, flag_id: FLAG_HANGAR, quantity: 1, singleton: true, group_id: 1, category_id: 1, custom_info: None })
}

async fn applied(ex: &(impl Executor + ?Sized), ship: i64) -> Result<bool, Error> {
    Ok(ex.query_opt("SELECT ship_item_id FROM game.ship_skin_applications WHERE ship_item_id=$1", &[&ship]).await?.is_some())
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn item_lifecycle_clears_only_invalid_applications() {
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    db.transaction::<(), (), Error, (), _, _, _>(|tx, _| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('skin-lifecycle-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let a: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'skin-lifecycle-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let b: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'skin-lifecycle-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        assert!(insert_license(tx, a, 1, None).await?);
        let mut ship = item(tx, a, 1).await?;
        assert!(set_application(tx, a, &ship, 1, 100).await?);
        let mut stale = ship.clone();
        stale.quantity += 1;
        assert!(!inventory::move_item(tx, &stale, 2, FLAG_HANGAR, b, true).await?);
        assert!(applied(tx, ship.item_id).await?);
        assert!(inventory::move_item(tx, &ship, 2, FLAG_HANGAR, a, true).await?);
        ship.location_id = 2;
        assert!(applied(tx, ship.item_id).await?);
        assert!(inventory::move_item(tx, &ship, 2, FLAG_HANGAR, b, true).await?);
        ship.owner_id = b;
        assert!(!applied(tx, ship.item_id).await?);
        assert!(inventory::move_item(tx, &ship, 2, FLAG_HANGAR, a, true).await?);
        ship.owner_id = a;
        assert!(!applied(tx, ship.item_id).await?);
        assert!(get_license(tx, a, 1, 100).await?.is_some());

        let root = item(tx, a, 3).await?;
        let nested = item(tx, a, root.item_id).await?;
        assert!(set_application(tx, a, &nested, 1, 100).await?);
        transfers::hand_over(tx, &[transfers::Transfer { item_id: root.item_id, owner: a, location: 4, flag: FLAG_HANGAR }]).await?;
        assert!(applied(tx, nested.item_id).await?);
        transfers::hand_over(tx, &[transfers::Transfer { item_id: root.item_id, owner: b, location: 4, flag: FLAG_HANGAR }]).await?;
        assert!(!applied(tx, nested.item_id).await?);

        let container = item(tx, a, 5).await?;
        let contents = item(tx, a, container.item_id).await?;
        assert!(set_application(tx, a, &contents, 1, 100).await?);
        exploration::set_container_owner(tx, container.item_id, b).await?;
        assert!(!applied(tx, contents.item_id).await?);

        assert!(set_application(tx, a, &ship, 1, 100).await?);
        let mut stale = ship.clone();
        stale.quantity += 1;
        assert_eq!(fitting::repackage(tx, &stale).await?, 0);
        assert!(applied(tx, ship.item_id).await?);
        assert_eq!(fitting::repackage(tx, &ship).await?, 1);
        assert!(!applied(tx, ship.item_id).await?);
        assert!(get_license(tx, a, 1, 100).await?.is_some());

        for id in [ship.item_id, nested.item_id, root.item_id, contents.item_id, container.item_id] {
            tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await?;
        }
        tx.execute("DELETE FROM game.characters WHERE character_id=ANY($1)", &[&&[a, b][..]]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
}
