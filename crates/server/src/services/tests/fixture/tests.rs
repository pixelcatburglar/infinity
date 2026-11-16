use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn fixture_lifecycle_is_scoped_and_selectable() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let a = create(&db).await.unwrap();
    let b = create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let sde = crate::tests::shared_sde(&db).await;
        for f in [a, b] {
            let row = crate::dao::characters::selection::selected(&db, f.character, f.account).await.unwrap().unwrap();
            let system = sde.map.system(row.solar_system_id).expect("fixture system must resolve in the SDE");
            assert_eq!((system.constellation_id, system.region_id), (Some(row.constellation_id), Some(row.region_id)));
            assert!(sde.map.constellation(row.constellation_id).is_some() && sde.map.region(row.region_id).is_some());
        }
        let item_a = crate::dao::items::creation::rookie_ship(&db, 1, a.character, 1, infinity_core::consts::FLAG_HANGAR).await.unwrap();
        let item_b = crate::dao::items::creation::rookie_ship(&db, 1, b.character, 1, infinity_core::consts::FLAG_HANGAR).await.unwrap();
        let before_a = crate::dao::tests::item_state(&db, &[item_a]).await.unwrap();
        assert!(crate::dao::items::dogma::set_override(&db, item_b, 1, 2.0).await.unwrap());
        assert_eq!(crate::dao::tests::item_state(&db, &[item_a]).await.unwrap(), before_a);
        assert!(crate::dao::items::dogma::set_override(&db, item_a, 1, 3.0).await.unwrap());
        assert_ne!(crate::dao::tests::item_state(&db, &[item_a]).await.unwrap(), before_a);
        let before_b = crate::dao::tests::item_state(&db, &[item_b]).await.unwrap();
        for f in [a, b] {
            crate::scheduler::schedule(&db, &crate::scheduler::Job {
                id: format!("{}:{}", crate::services::skills::completion::KIND, f.character),
                kind: crate::services::skills::completion::KIND.into(), due_at: i64::MAX, payload: f.character.to_string(),
            }).await.unwrap();
        }
        let rows_a = capture_fixture_rows(&db, &[a], &[item_a]).await.unwrap();
        let rows_b = capture_fixture_rows(&db, &[b], &[item_b]).await.unwrap();
        cleanup(&db, a).await.unwrap();
        assert_fixture_removed(&db, &rows_a).await.unwrap();
        assert!(crate::dao::characters::selection::selected(&db, b.character, b.account).await.unwrap().is_some());
        assert_eq!(crate::dao::tests::item_state(&db, &[item_b]).await.unwrap(), before_b);
        assert!(db.query_opt("SELECT 1 FROM game.jobs WHERE job_id = $1",
            &[&format!("{}:{}", crate::services::skills::completion::KIND, b.character)]).await.unwrap().is_some());
        cleanup(&db, b).await.unwrap();
        assert_fixture_removed(&db, &rows_b).await.unwrap();
    }).await;
    cleanup(&db, b).await.unwrap();
    cleanup(&db, a).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
