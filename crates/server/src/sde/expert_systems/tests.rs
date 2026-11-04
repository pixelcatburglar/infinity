use super::*;
use crate::dao::sde::expert_systems::ExpertSystemRow;
use std::collections::HashMap;

fn categories(entries: &[(i32, i32)]) -> HashMap<i32, i32> {
    entries.iter().copied().collect()
}

fn valid_json() -> &'static str {
    r#"{"_key":1000,"durationDays":3,"skillsGranted":{"3001":4,"3002":2},"associatedShipTypes":[4001],"esHidden":false,"esRetired":true}"#
}

#[test]
fn parses_valid_row_and_converts_duration() {
    let known = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (3001, CATEGORY_SKILL), (3002, CATEGORY_SKILL), (4001, CATEGORY_SHIP)]);
    let category = |id| known.get(&id).copied();
    let parsed = parse_row(&Some("1000".into()), valid_json(), &category).unwrap();
    assert_eq!(parsed.type_id, 1000);
    assert_eq!(parsed.duration_days, 3);
    assert_eq!(parsed.duration_filetime, 3 * FILETIME_DAY);
    assert_eq!(parsed.skills, vec![(3001, 4), (3002, 2)]);
    assert_eq!(parsed.associated_ship_types, vec![4001]);
    assert!(!parsed.hidden);
    assert!(parsed.retired);
}

#[test]
fn parses_absent_optional_ship_list_as_empty() {
    let known = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (3001, CATEGORY_SKILL)]);
    let category = |id| known.get(&id).copied();
    let data = r#"{"_key":1000,"durationDays":3,"skillsGranted":{"3001":4},"esHidden":false,"esRetired":true}"#;
    let parsed = parse_row(&Some("1000".into()), data, &category).unwrap();
    assert!(parsed.associated_ship_types.is_empty());
}

#[test]
fn rejects_present_nonarray_ship_list() {
    let known = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (3001, CATEGORY_SKILL)]);
    let category = |id| known.get(&id).copied();
    let data = r#"{"_key":1000,"durationDays":3,"skillsGranted":{"3001":4},"associatedShipTypes":null,"esHidden":false,"esRetired":true}"#;
    let error = parse_row(&Some("1000".into()), data, &category).unwrap_err().to_string();
    assert!(error.contains("associatedShipTypes must be an array"), "{error}");
}

#[test]
fn rejects_duplicate_numeric_skill_keys() {
    let known = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (3001, CATEGORY_SKILL), (4001, CATEGORY_SHIP)]);
    let category = |id| known.get(&id).copied();
    let data = r#"{"_key":1000,"durationDays":1,"skillsGranted":{"3001":2,"03001":4},"associatedShipTypes":[4001],"esHidden":false,"esRetired":false}"#;
    let error = parse_row(&Some("1000".into()), data, &category).unwrap_err().to_string();
    assert!(error.contains("duplicate skill ID 3001"), "{error}");
}

#[test]
fn rejects_unknown_category_and_references() {
    let no_expert_category = categories(&[(1000, CATEGORY_SKILL), (3001, CATEGORY_SKILL), (4001, CATEGORY_SHIP)]);
    let category = |id| no_expert_category.get(&id).copied();
    assert!(parse_row(&Some("1000".into()), valid_json(), &category).is_err());

    let unknown_skill = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (4001, CATEGORY_SHIP)]);
    let category = |id| unknown_skill.get(&id).copied();
    let error = parse_row(&Some("1000".into()), valid_json(), &category).unwrap_err().to_string();
    assert!(error.contains("references missing skill 3001"), "{error}");

    let unknown_ship = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (3001, CATEGORY_SKILL), (3002, CATEGORY_SKILL)]);
    let category = |id| unknown_ship.get(&id).copied();
    let error = parse_row(&Some("1000".into()), valid_json(), &category).unwrap_err().to_string();
    assert!(error.contains("references missing ship 4001"), "{error}");
}

#[test]
fn rejects_malformed_rows() {
    let known = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (3001, CATEGORY_SKILL), (4001, CATEGORY_SHIP)]);
    let category = |id| known.get(&id).copied();
    for (key, data) in [
        (Some("1000".into()), "not-json"),
        (Some("1000".into()), r#"{"_key":1000,"durationDays":1,"skillsGranted":{},"associatedShipTypes":[],"esHidden":false}"#),
        (None, valid_json()),
    ] {
        assert!(parse_row(&key, data, &category).is_err(), "{data}");
    }
}

#[test]
fn rejects_duplicate_expert_system_rows() {
    let known = categories(&[(1000, CATEGORY_EXPERT_SYSTEM), (3001, CATEGORY_SKILL), (3002, CATEGORY_SKILL), (4001, CATEGORY_SHIP)]);
    let category = |id| known.get(&id).copied();
    let row = |key: &str| ExpertSystemRow { key: Some(key.into()), data: valid_json().into() };
    let error = match ExpertSystems::from_rows(vec![row("1000"), row("1000")], &category) {
        Ok(_) => panic!("duplicate expert-system rows were accepted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("duplicate expert-system type ID"), "{error}");
}
