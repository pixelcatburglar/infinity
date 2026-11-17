use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn captured_custody_shape_includes_physical_identity_and_online_state() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let id: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES (1,2,3,4,5,false,true) RETURNING item_id", &[]).await.unwrap().get("item_id");
    let item = ItemId::new(id).unwrap();
    let rows = capture_many(&tx, &[item]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].item, rows[0].owner, rows[0].location, rows[0].quantity), (item, 2, 3, 5));
    assert_eq!(online(&tx, &[item]).await.unwrap(), vec![item]);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn withdrawal_rekeys_attributes_and_service_life_transactionally() {
    use crate::dao::items::parent_locks::{self, LockRequest, LockStrength};
    use crate::dao::repairable_ship::{self as recovery, LossSource, NewGroup, NewItem, SourceKind, Status};
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let seq: i64 = tx.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let account = crate::dao::accounts::authentication::register(&tx, &format!("recovery-test-{seq}"), b"fixture", 0, 0).await.unwrap().user_id;
    let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'recovery-test-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let raw: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,3,4,1,true) RETURNING item_id", &[&owner]).await.unwrap().get("item_id");
    let old = ItemId::new(raw).unwrap();
    let shape = capture_many(&tx, &[old]).await.unwrap().remove(0);
    let group_id = recovery::insert_group(&tx, &NewGroup { loss: LossSource { kind: SourceKind::UnpilotedShip,
        beneficiary: owner, shape: shape.clone(), system: 3, lethal_at: 1, sampled_position: [0.0; 3] },
        loss_id: None, destination_station: 3, payment_expires_at: 100 }).await.unwrap();
    let locks = parent_locks::lock(&tx, &[], &[LockRequest { item_id: raw, strength: LockStrength::Update }]).await.unwrap();
    assert!(move_to_custody(&tx, &locks, group_id, &[shape]).await.unwrap());
    let member_id = recovery::insert_items(&tx, &[NewItem { group: group_id, item: old, original_owner: owner,
        type_id: 1, flag: 4, quantity: 1, singleton: true, last_service_life: 5.0, repair_completed_at: 1 }]).await.unwrap()[0];
    assert_eq!(recovery::escrow_beneficiary(&tx, old).await.unwrap(), Some(owner));
    tx.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&raw, &owner]).await.unwrap();
    assert_eq!(recovery::escrow_beneficiary(&tx, old).await.unwrap(), None);
    tx.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&raw, &infinity_core::consts::NPC_CORP_INTERBUS]).await.unwrap();
    let payment = recovery::Payment { member: member_id, amount: crate::isk::IskAmount::ZERO };
    assert_eq!(recovery::start_delivery(&tx, 1, 1, 1, &[payment]).await.unwrap(), vec![member_id]);
    tx.execute("INSERT INTO game.item_service_life (item_id,damage) VALUES ($1,5)", &[&raw]).await.unwrap();
    tx.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value,updated_at,cargo_unlimited) VALUES ($1,$2,10,42,true)", &[&raw, &infinity_core::consts::dogma::ATTR_CAPACITY]).await.unwrap();
    tx.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,7)", &[&raw, &infinity_core::consts::dogma::ATTR_HULL_DAMAGE]).await.unwrap();
    tx.execute("INSERT INTO game.dynamic_items (item_id,source_item_id,mutator_item_id,source_type_id,mutator_type_id,result_type_id,creator_id,created_at) VALUES ($1,10,11,1,1,1,$2,1)", &[&raw, &owner]).await.unwrap();
    tx.execute("INSERT INTO game.dynamic_item_modifiers (item_id,attribute_id,factor) VALUES ($1,$2,1.1)", &[&raw, &infinity_core::consts::dogma::ATTR_CAPACITY]).await.unwrap();
    tx.execute("INSERT INTO game.ship_configuration (ship_item_id,owner_id) VALUES ($1,$2)", &[&raw, &owner]).await.unwrap();
    tx.execute("INSERT INTO game.ship_dirt (ship_item_id,dirt_timestamp) VALUES ($1,1)", &[&raw]).await.unwrap();
    tx.execute("INSERT INTO game.ship_kill_counts (ship_item_id,player_kills) VALUES ($1,3)", &[&raw]).await.unwrap();
    tx.execute("INSERT INTO game.ship_skin_licenses (character_id,skin_id,expires_at) VALUES ($1,1,100)", &[&owner]).await.unwrap();
    tx.execute("INSERT INTO game.ship_skin_applications (ship_item_id,character_id,skin_id) VALUES ($1,$2,1)", &[&raw, &owner]).await.unwrap();
    let group = recovery::get_group(&tx, group_id).await.unwrap().unwrap();
    let member = recovery::get_member(&tx, member_id).await.unwrap().unwrap();
    let selection = Selection { group, items: vec![member] };
    let before: i64 = tx.query_one("SELECT count(*) AS n FROM game.items WHERE type_id=1 AND owner_id=$1 AND location_id=3", &[&owner]).await.unwrap().get("n");
    tx.execute("CREATE FUNCTION pg_temp.fail_recovery_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected failure'; END; $$", &[]).await.unwrap();
    for table in ["item_attributes", "dynamic_items", "dynamic_item_modifiers", "item_service_life", "ship_configuration", "ship_skin_applications", "ship_dirt", "ship_kill_counts"] {
        tx.execute("SAVEPOINT recovery_failure", &[]).await.unwrap();
        tx.execute(&format!("CREATE TRIGGER recovery_failure BEFORE INSERT ON game.{table} FOR EACH ROW EXECUTE FUNCTION pg_temp.fail_recovery_insert()"), &[]).await.unwrap();
        assert!(restore_new(&tx, &locks, &selection, 1).await.is_err());
        tx.execute("ROLLBACK TO SAVEPOINT recovery_failure", &[]).await.unwrap();
        let after: i64 = tx.query_one("SELECT count(*) AS n FROM game.items WHERE type_id=1 AND owner_id=$1 AND location_id=3", &[&owner]).await.unwrap().get("n");
        assert_eq!(before, after);
        assert_eq!(recovery::get_member(&tx, member_id).await.unwrap().unwrap().status, Status::Delivering);
    }
    tx.execute("SAVEPOINT expired_skin", &[]).await.unwrap();
    tx.execute("UPDATE game.ship_skin_licenses SET expires_at=0 WHERE character_id=$1 AND skin_id=1", &[&owner]).await.unwrap();
    let no_skin = restore_new(&tx, &locks, &selection, 1).await.unwrap().unwrap();
    assert!(tx.query_opt("SELECT ship_item_id FROM game.ship_skin_applications WHERE ship_item_id=$1", &[&no_skin.get()]).await.unwrap().is_none());
    tx.execute("ROLLBACK TO SAVEPOINT expired_skin", &[]).await.unwrap();
    let new = restore_new(&tx, &locks, &selection, 1).await.unwrap().unwrap();
    assert_ne!(new, old);
    assert_eq!(recovery::terminalize(&tx, &[(member_id, Some(new))], &[Status::Delivering], Status::Withdrawn, 2).await.unwrap(), vec![member_id]);
    assert!(delete_custody(&tx, &locks, &selection).await.unwrap());
    let damage: f64 = tx.query_one("SELECT damage FROM game.item_service_life WHERE item_id=$1", &[&new.get()]).await.unwrap().get("damage");
    let attr = tx.query_one("SELECT value,updated_at,cargo_unlimited FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2", &[&new.get(), &infinity_core::consts::dogma::ATTR_CAPACITY]).await.unwrap();
    assert_eq!((attr.get::<_, f64>("value"), attr.get::<_, i64>("updated_at"), attr.get::<_, bool>("cargo_unlimited")), (10.0, 42, true));
    assert!(tx.query_opt("SELECT item_id FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2", &[&new.get(), &infinity_core::consts::dogma::ATTR_HULL_DAMAGE]).await.unwrap().is_none());
    let dynamic = tx.query_one("SELECT source_item_id,mutator_item_id FROM game.dynamic_items WHERE item_id=$1", &[&new.get()]).await.unwrap();
    assert_eq!((dynamic.get::<_, i64>("source_item_id"), dynamic.get::<_, i64>("mutator_item_id")), (10, 11));
    assert!(tx.query_opt("SELECT item_id FROM game.dynamic_item_modifiers WHERE item_id=$1", &[&new.get()]).await.unwrap().is_some());
    assert!(tx.query_opt("SELECT ship_item_id FROM game.ship_configuration WHERE ship_item_id=$1 AND owner_id=$2", &[&new.get(), &owner]).await.unwrap().is_some());
    assert!(tx.query_opt("SELECT ship_item_id FROM game.ship_skin_applications WHERE ship_item_id=$1 AND character_id=$2 AND skin_id=1", &[&new.get(), &owner]).await.unwrap().is_some());
    assert!(tx.query_opt("SELECT ship_item_id FROM game.ship_dirt WHERE ship_item_id=$1", &[&new.get()]).await.unwrap().is_some());
    assert!(tx.query_opt("SELECT ship_item_id FROM game.ship_kill_counts WHERE ship_item_id=$1 AND player_kills=3", &[&new.get()]).await.unwrap().is_some());
    assert_eq!(damage, 5.0);
    tx.rollback().await.unwrap();
}
