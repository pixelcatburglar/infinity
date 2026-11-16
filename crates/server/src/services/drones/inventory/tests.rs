use super::*;

use crate::services::drones::tests::fixture::Fixture;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn abandon_commits_neutral_state_with_closed_pilot_receiver() {
    let mut f = Fixture::new().await;
    let mut fixture_id = None;
    let result = crate::tests::catch_scenario(async {
    let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
    let id = crate::dao::items::creation::positioned(&f.db, ty, f.owner, f.system, [0.0; 3]).await.unwrap();
    fixture_id = Some(id);
    f.db.execute("UPDATE game.characters SET gm_npc_ignored=true WHERE character_id=$1", &[&f.owner]).await.unwrap();
    let entries = restore_specs(&f.db, f.sde.clone(), f.system).await.ok().unwrap();
    let effects = super::super::worker::effects(f.db.clone(), Arc::downgrade(&f.world), f.system);
    f.world.with(f.system, move |s| { for prepared in entries { s.restore_drone(prepared, effects.clone()); } }).await.unwrap();
    assert!(f.world.with(f.system, move |s| s.npc_target_ignored(id)).await.unwrap());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);
    f.world.register_pilot(f.owner, tx);
    abandon(&mut f.client, id).await.ok().unwrap();
    let row = f.db.query_one("SELECT owner_id, location_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("owner_id"), OWNER_UNOWNED);
    assert_eq!(row.get::<_, i64>("location_id"), f.system);
    assert!(f.world.with(f.system, move |s| s.ball(id).is_some_and(|b| b.owner_id == OWNER_UNOWNED) && s.drone(id).is_some_and(|d| d.owner == OWNER_UNOWNED && d.controller.is_none()) && !s.npc_target_ignored(id)).await.unwrap());
    assert!(abandon(&mut f.client, id).await.is_err());
    }).await;
    if let Some(id) = fixture_id {
        f.db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
        f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn hull_admission_ignores_stack_fields_and_refuses_a_moved_ship() {
    use marshal::Value;
    let mut f = Fixture::new().await;
    let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
        && t.attribute(infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).unwrap().type_id;
    f.train_drone(ty).await; f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
    f.db.execute("UPDATE game.items SET flag_id=9, quantity=3 WHERE item_id=$1", &[&f.ship]).await.unwrap();
    let launch = |id: i64| [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
    let singleton = f.drone(ty, 1, true).await; let first = f.call("ship", "LaunchDrones", &launch(singleton)).await.ok().unwrap();
    f.db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&f.ship, &(f.system + 1)]).await.unwrap();
    let refused = f.drone(ty, 1, true).await; let second = f.call("ship", "LaunchDrones", &launch(refused)).await.ok().unwrap();
    f.cleanup().await;
    assert_eq!(first, Value::Dict(vec![(Value::Int(singleton), Value::list([Value::Int(singleton)]))]));
    let notify = Value::dict([("notify", Value::unicode(infinity_core::messages::drones::NOT_AVAILABLE))]);
    let error = Value::list([Value::tuple([Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY), notify])]);
    assert_eq!(second, Value::Dict(vec![(Value::Int(refused), error)]));
}
