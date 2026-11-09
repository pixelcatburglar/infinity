use super::*;

#[test]
fn cargo_requirements_aggregate_by_station_and_type_with_checked_boundaries() {
    let mut goods = BTreeMap::new();
    assert!(add_requirement(&mut goods, 1, 2, 10).is_ok());
    assert!(add_requirement(&mut goods, 1, 2, 10).is_ok());
    assert!(add_requirement(&mut goods, 2, 2, 10).is_ok());
    assert!(add_requirement(&mut goods, 1, 3, 10).is_ok());
    assert_eq!(goods[&(1, 2)], 20);
    assert_eq!(goods[&(2, 2)], 10);
    assert_eq!(goods[&(1, 3)], 10);
    assert_eq!(missing_amount(goods[&(1, 2)], 10), 10);
    assert_eq!(missing_amount(goods[&(1, 2)], 25), 0);
    assert_eq!(missing_amount(i64::from(i32::MAX) + 1, 0), i64::from(i32::MAX) + 1);
    assert_eq!(missing_amount(10, i128::from(i64::MAX) * 2), 0);
    assert_eq!(missing_amount(10, -1), 10);
    assert!(add_requirement(&mut goods, 3, 4, i64::MAX).is_ok());
    assert!(add_requirement(&mut goods, 3, 4, 1).is_err());
    assert_eq!(goods[&(3, 4)], i64::MAX);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn force_complete_merges_missing_courier_and_trade_goods() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let step = c.sde.missions.steps().find(|step| super::super::agent(step.agent).is_some()
            && c.sde.missions.get(step.mission_id).is_some()).expect("mission step with retail agent");
        let agent = super::super::agent(step.agent).unwrap();
        fixture::seed_mission(&test_db, &c.sde, f.character, agent.id, agent.station_id, agent.system_id, step.mission_id, step.arc).await.unwrap();
        fixture::clear_cargo_stack(&test_db, f.character).await.unwrap();
        let type_id = c.sde.types.iter().find(|t| t.published == Some(true) && c.sde.is_tradable(t.type_id))
            .expect("tradable type").type_id;
        let initial = 3_i64;
        let required = 10_i32;
        let stack = crate::dao::items::transfers::insert(&test_db, type_id, f.character, agent.station_id, FLAG_HANGAR, initial).await.unwrap();
        for kind in ["courier", "trade"] {
            test_db.execute("UPDATE game.agent_missions SET state=$3, kind=$4, arc_id=NULL, dest_agent=NULL, cargo_type=$5, cargo_qty=$6 WHERE character_id=$1 AND agent_id=$2",
                &[&f.character, &agent.id, &STATE_ACCEPTED, &kind, &type_id, &required]).await.unwrap();
            assert_eq!(force_complete(&mut c, Some(agent.id)).await.unwrap(), 1);
            let rows: Vec<_> = items::load_items(&test_db, &c.sde, ItemQuery::OwnerLocationFlag(f.character, agent.station_id, FLAG_HANGAR)).await.unwrap()
                .into_iter().filter(|item| item.type_id == type_id && !item.singleton).collect();
            assert_eq!(rows.len(), 1);
            assert_eq!((rows[0].item_id, rows[0].quantity), (stack, i64::from(required)));
            test_db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&stack, &initial]).await.unwrap();
        }
        test_db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&stack, &i64::MAX]).await.unwrap();
        crate::dao::items::transfers::insert(&test_db, type_id, f.character, agent.station_id, FLAG_HANGAR, i64::MAX).await.unwrap();
        assert_eq!(force_complete(&mut c, Some(agent.id)).await.unwrap(), 1);
        let rows: Vec<_> = items::load_items(&test_db, &c.sde, ItemQuery::OwnerLocationFlag(f.character, agent.station_id, FLAG_HANGAR)).await.unwrap()
            .into_iter().filter(|item| item.type_id == type_id && !item.singleton).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|item| item.quantity == i64::MAX));
    }).await;
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    result.unwrap();
}
