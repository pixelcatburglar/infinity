//! Shared inventory lifecycle regressions: the same primitive backs ordinary
//! trash and impound release/trash, so closure safety cases live here once.

use super::*;


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn corporate_closure_refuses_foreign_descendant() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("test SDE has a station").station_id;
    let ty = sde.types.iter().find(|ty| ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP)
        && !sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), ty.category_id.unwrap_or_default()))
        .expect("test SDE has a trashable non-ship type").type_id;
    let parent = crate::dao::items::transfers::insert(&db, ty, fixture.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let foreign = crate::dao::items::transfers::insert(&db, ty, fixture.character + 1, parent, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
    let lineages = std::collections::HashMap::from([
        (parent, crate::dao::items::deliveries::ancestry(&db, parent).await.unwrap()),
        (foreign, crate::dao::items::deliveries::ancestry(&db, foreign).await.unwrap()),
    ]);
    let snapshots = load_items(&db, &sde, ItemQuery::Ids(&[parent, foreign])).await.unwrap();
    let prepared = Prepared {
        roots: vec![parent],
        snapshots,
        lineages,
        outer: station,
        boundary: None,
        endpoints: Vec::new(),
        reach: None,
        shared_ids: std::collections::HashSet::new(),
        active_ship: None,
        station: None,
        notify: None,
    };
    let custody = CustodyRule::Corporate { corporation: fixture.character };
    let result = db.transaction(
        |tx, _: &mut ()| {
            let prepared = prepared.clone();
            let custody = custody;
            let sde = sde.clone();
            Box::pin(async move {
                verify_closure(tx, &sde, &prepared, &custody, Operation::Destroy).await.map(|_| ())
            })
        },
        |value, ()| async move { Ok::<_, CallError>(value) },
    ).await;
    assert!(result.is_err(), "a foreign descendant must fail closed");
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id = ANY($1)", &[&vec![parent, foreign]]).await.unwrap().get::<_, i64>("n"), 2);
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&foreign, &fixture.character]).await.unwrap();
    db.execute("INSERT INTO game.industry_jobs (installer_id,owner_id,activity_id,blueprint_id,blueprint_type_id,blueprint_copy,facility_id,solar_system_id,runs,cost,time_seconds,start_date,end_date,status) VALUES ($1,$1,1,$2,$3,false,$4,0,1,0,1,0,1,$5)", &[&fixture.character, &foreign, &ty, &station, &infinity_core::consts::INDUSTRY_JOB_STATUS_INSTALLED]).await.unwrap();
    let mut with_job = prepared.clone();
    with_job.snapshots = load_items(&db, &sde, ItemQuery::Ids(&[parent, foreign])).await.unwrap();
    with_job.lineages.insert(foreign, crate::dao::items::deliveries::ancestry(&db, foreign).await.unwrap());
    let result = db.transaction(
        |tx, _: &mut ()| {
            let prepared = with_job.clone();
            let sde = sde.clone();
            Box::pin(async move { verify_closure(tx, &sde, &prepared, &custody, Operation::Release).await.map(|_| ()) })
        },
        |value, ()| async move { Ok::<_, CallError>(value) },
    ).await;
    assert!(result.is_err(), "an installed job must prevent release");
    db.execute("DELETE FROM game.industry_jobs WHERE blueprint_id=$1", &[&foreign]).await.unwrap();
    let hull_type = sde.types.iter().find(|row| row.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|row| row.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let charge_type = sde.types.iter().find(|row| row.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    let hull = crate::dao::items::creation::rookie_ship(&db, hull_type, fixture.character, station, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    let module = crate::dao::items::creation::rookie_ship(&db, module_type, fixture.character, hull, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap();
    let reload = pending_reloads::insert(&db, fixture.character, hull, module, infinity_core::consts::FLAG_FITTING_FIRST, charge_type, 1, infinity_core::consts::FLAG_CARGO, foreign).await.unwrap();
    let result = db.transaction(
        |tx, _: &mut ()| {
            let prepared = with_job.clone();
            let sde = sde.clone();
            Box::pin(async move { verify_closure(tx, &sde, &prepared, &custody, Operation::Release).await.map(|_| ()) })
        },
        |value, ()| async move { Ok::<_, CallError>(value) },
    ).await;
    assert!(result.is_err(), "a pending reload from the subtree must prevent release");
    pending_reloads::take(&db, reload).await.unwrap();
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&parent, &foreign]).await.unwrap();
    let mut cyclic = with_job;
    cyclic.snapshots = load_items(&db, &sde, ItemQuery::Ids(&[parent, foreign])).await.unwrap();
    cyclic.lineages.insert(parent, crate::dao::items::deliveries::ancestry(&db, parent).await.unwrap());
    cyclic.lineages.insert(foreign, crate::dao::items::deliveries::ancestry(&db, foreign).await.unwrap());
    let result = db.transaction(
        |tx, _: &mut ()| {
            let prepared = cyclic.clone();
            let sde = sde.clone();
            Box::pin(async move { verify_closure(tx, &sde, &prepared, &custody, Operation::Destroy).await.map(|_| ()) })
        },
        |value, ()| async move { Ok::<_, CallError>(value) },
    ).await;
    assert!(result.is_err(), "a cyclic subtree must fail closed");
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![parent, foreign]]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
