use super::*;
use crate::services::tests::fixture::{self, Fixture};
use infinity_core::consts::{FILETIME_SECOND, FILETIME_UNIX_EPOCH};

fn was_null(error: &db::Error) -> bool {
    let db::Error::Postgres(postgres) = error else { return false };
    std::error::Error::source(postgres).is_some_and(|source| source.is::<tokio_postgres::types::WasNull>())
}

async fn check(db: &crate::db::Db, fixture: Fixture) {
    let original = selected(db, fixture.character, fixture.account).await.unwrap().unwrap();
    let (system, constellation, region) = (original.solar_system_id, original.constellation_id, original.region_id);
    assert!(original.station_id.is_none() && original.roles == 0 && original.authority.corporation == 1 && !original.authority.live);
    for (column, others, left, right) in [("solar_system_id", "constellation_id = $2, region_id = $3", constellation, region), ("constellation_id", "solar_system_id = $2, region_id = $3", system, region), ("region_id", "solar_system_id = $2, constellation_id = $3", system, constellation)] {
        db.execute(&format!("UPDATE game.characters SET {column} = NULL, {others} WHERE character_id = $1"), &[&fixture.character, &left, &right]).await.unwrap();
        match selected(db, fixture.character, fixture.account).await { Ok(_) => panic!("{column} NULL was accepted"), Err(error) => assert!(was_null(&error), "{column}") }
        if column == "solar_system_id" {
            let rows = characters(db, fixture.account, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.unwrap();
            assert_eq!(rows.iter().find(|row| row.character_id == fixture.character).map(|row| row.solar_system_id), Some(None));
            assert!(rows.iter().all(|row| row.balance.is_finite()));
        }
    }
    assert!(matches!(selected(db, fixture.character, fixture.account.wrapping_neg()).await, Ok(None)));
    assert_eq!(crate::dao::characters::ship::location(db, fixture.character, system, constellation, region).await.unwrap(), 1);
    let restored = selected(db, fixture.character, fixture.account).await.unwrap().unwrap();
    assert_eq!((restored.solar_system_id, restored.constellation_id, restored.region_id), (system, constellation, region));
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database with initialized SDE"]
async fn selection_reports_null_location_without_hiding_list_rows() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = fixture::create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(check(&db, fixture)).await;
    let cleanup = crate::tests::catch_scenario(async { fixture::cleanup(&db, fixture).await.unwrap(); }).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
}
