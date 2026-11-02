use super::*;

fn row(id: i32, json: &str) -> dao::GraphicLocationRow {
    dao::GraphicLocationRow { type_id: id, directional_locators: json.into() }
}

#[test]
fn parses_locators_and_rejects_invalid_vectors() {
    let data = GraphicLocations::from_rows(vec![row(1, r#"[{"name":"synthetic","category":"undockPointSmall","position":[1,2,3],"direction":[0,0,2]}]"#), row(2, "[]"), row(3, "null")]).unwrap();
    assert_eq!(data.get(1)[0].position, [1.0, 2.0, 3.0]);
    assert_eq!(data.get(1)[0].direction, [0.0, 0.0, 2.0]);
    assert_eq!(data.get(1)[0].category, "undockPointSmall");
    assert!(data.get(2).is_empty()); assert!(data.get(3).is_empty()); assert!(data.get(4).is_empty());
    for json in ["{}", "[{}]", r#"[{"category":"x","position":[0,0],"direction":[0,0,1]}]"#, r#"[{"category":"x","position":[0,0,0],"direction":[0,0,0]}]"#, r#"[{"category":"x","position":[0,0,0],"direction":[0,"bad",1]}]"#, r#"[{"category":"x","position":[1e400,0,0],"direction":[0,0,1]}]"#] {
        assert!(GraphicLocations::from_rows(vec![row(1, json)]).is_err(), "{json}");
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn loads_generic_and_size_separated_station_locators() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let mut generic = false;
    let mut sized = false;
    for station in sde.map.stations() {
        let Some(id) = station.type_id.and_then(|id| i32::try_from(id).ok()) else { continue; };
        let locators = sde.graphic_locations.get(id);
        if !locators.is_empty() && locators.iter().all(|l| l.category == "undockPoint") { generic = true; }
        if locators.iter().any(|l| l.category == "undockPointSmall") && locators.iter().any(|l| l.category == "undockPointLarge") && locators.iter().any(|l| l.category == "undockPointCapital") { sized = true; }
        for locator in locators {
            assert!(locator.position.iter().chain(locator.direction.iter()).all(|v| v.is_finite()));
            assert_ne!(locator.direction, [0.0; 3]);
        }
    }
    assert!(generic && sized, "expected both generic and size-separated NPC station locators");
}
