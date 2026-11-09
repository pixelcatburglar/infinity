//! Deployment geometry and guarded physical inventory placement.
use super::super::deploy::position;
use infinity_core::{consts::{GROUP_CONTROL_TOWER, GROUP_SUN, TYPE_SOVEREIGNTY_HUB}};
use crate::{dao::items::{loading::Item, sovereignty}, sde::sovereignty::DeployDefinition, services::sovereignty::{PlacementObstacle, PlacementProof}};

#[test]
fn fixed_star_position_and_obstacle_boundary() {
    let rules = DeployDefinition { fixed_star_distance_m: 10, min_distance_from_control_tower_m: 2.0, ..Default::default() };
    let mut proof = PlacementProof { ship_id: 1, character_id: 1, position: [1.0, 0.0, 0.0], obstacles: vec![PlacementObstacle { group_id: GROUP_SUN, position: [0.0; 3] }] };
    assert_eq!(position(&proof, &rules), Some([10.0, 0.0, 0.0]));
    proof.obstacles.push(PlacementObstacle { group_id: GROUP_CONTROL_TOWER, position: [12.0, 0.0, 0.0] });
    assert_eq!(position(&proof, &rules), Some([10.0, 0.0, 0.0]));
    proof.obstacles[1].position[0] = 11.0;
    assert_eq!(position(&proof, &rules), None);
    proof.position = [0.0; 3];
    assert_eq!(position(&proof, &rules), None);
}

#[test]
fn placement_requires_exactly_one_star() {
    let rules = DeployDefinition { fixed_star_distance_m: 10, ..Default::default() };
    let mut proof = PlacementProof { ship_id: 1, character_id: 1, position: [1.0, 0.0, 0.0], obstacles: Vec::new() };
    assert_eq!(position(&proof, &rules), None);
    proof.obstacles.push(PlacementObstacle { group_id: GROUP_SUN, position: [0.0; 3] });
    assert_eq!(position(&proof, &rules), Some([10.0, 0.0, 0.0]));
    proof.obstacles.push(PlacementObstacle { group_id: GROUP_SUN, position: [0.0; 3] });
    assert_eq!(position(&proof, &rules), None);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn real_hub_item_placement_is_guarded_and_transactional() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { let _ = connection.await; });
    let tx = client.transaction().await.unwrap();
    let id: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,2,3,4,1,false) RETURNING item_id", &[&TYPE_SOVEREIGNTY_HUB]).await.unwrap().get("item_id");
    let item = Item { item_id: id, type_id: TYPE_SOVEREIGNTY_HUB, owner_id: 2, location_id: 3, flag_id: 4, quantity: 1, singleton: false, group_id: 0, category_id: 0, custom_info: None };
    let mut stale = item.clone();
    stale.quantity = 2;
    assert!(!sovereignty::place(&tx, &stale, 5, 6, [1.0, 2.0, 3.0]).await.unwrap());
    assert!(sovereignty::place(&tx, &item, 5, 6, [1.0, 2.0, 3.0]).await.unwrap());
    assert!(!sovereignty::place(&tx, &item, 5, 6, [1.0, 2.0, 3.0]).await.unwrap());
    let row = tx.query_one("SELECT owner_id,location_id,quantity,singleton,x,y,z FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i64>("quantity"), row.get::<_, bool>("singleton")), (6, 5, 1, true));
    assert_eq!((row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")), (1.0, 2.0, 3.0));
    tx.rollback().await.unwrap();
}
