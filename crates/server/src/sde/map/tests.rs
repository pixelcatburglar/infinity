use super::*;

fn row(owner: i64, services: &str) -> dao::StationStandingRestriction {
    dao::StationStandingRestriction { owner: Some(owner.to_string()), services: Some(services.to_owned()) }
}

fn map_with(restrictions: Vec<dao::StationStandingRestriction>) -> Map {
    Map {
        systems: Vec::new(), regions: Vec::new(), constellations: Vec::new(),
        stargates: Vec::new(), stations: Vec::new(), celestials: Vec::new(),
        system_index: HashMap::new(), region_index: HashMap::new(),
        constellation_index: HashMap::new(), stargate_index: HashMap::new(),
        station_index: HashMap::new(), celestial_index: HashMap::new(),
        system_celestials: HashMap::new(), owner_stations: HashMap::new(),
        jumps: HashMap::new(),
        station_standing_restrictions: project_station_standing_restrictions(restrictions).unwrap(),
    }
}

#[test]
fn empty_and_missing_restrictions_are_unrestricted() {
    let map = map_with(vec![row(42, "{}")]);
    assert_eq!(map.required_station_standing(42, 7), None);
    assert_eq!(map.required_station_standing(43, 7), None);
}

#[test]
fn exempt_owner_threshold_is_available() {
    let owner = STANDING_SKILL_EXEMPT_FACTIONS[0];
    let map = map_with(vec![row(owner, r#"{"7":1.25}"#)]);
    assert_eq!(map.required_station_standing(owner, 7), Some(1.25));
    assert_eq!(map.required_station_standing(owner, 8), None);
}

#[test]
fn malformed_services_and_thresholds_are_rejected() {
    let owner = STANDING_SKILL_EXEMPT_FACTIONS[0];
    for services in ["null", "[]", r#"{"invalid":1}"#, r#"{"7":"1"}"#, r#"{"7":null}"#, r#"{"7":"NaN"}"#, r#"{"7":1e999}"#] {
        assert!(project_station_standing_restrictions(vec![row(owner, services)]).is_err(), "accepted {services}");
    }
}

#[test]
fn populated_nonexempt_owner_is_rejected_with_context() {
    let error = project_station_standing_restrictions(vec![row(42, r#"{"7":1}"#)]).unwrap_err().to_string();
    assert!(error.contains("42"), "{error}");
    assert!(error.contains("7"), "{error}");
}
