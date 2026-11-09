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
    let entries = restore_specs(&f.db, f.sde.clone(), f.system).await.ok().unwrap();
    let effects = super::super::worker::effects(f.db.clone(), Arc::downgrade(&f.world), f.system);
    f.world.with(f.system, move |s| { for prepared in entries { s.restore_drone(prepared, effects.clone()); } }).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);
    f.world.register_pilot(f.owner, tx);
    abandon(&mut f.client, id).await.ok().unwrap();
    let row = f.db.query_one("SELECT owner_id, location_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("owner_id"), OWNER_UNOWNED);
    assert_eq!(row.get::<_, i64>("location_id"), f.system);
    assert!(f.world.with(f.system, move |s| s.ball(id).is_some_and(|b| b.owner_id == OWNER_UNOWNED) && s.drone(id).is_some_and(|d| d.owner == OWNER_UNOWNED && d.controller.is_none())).await.unwrap());
    assert!(abandon(&mut f.client, id).await.is_err());
    }).await;
    if let Some(id) = fixture_id {
        f.db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
        f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
