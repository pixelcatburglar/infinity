use super::*;

fn parse(text: &str) -> Result<BTreeMap<i32, Policy>> {
    parse_with_type_exists(text, |id| id > 0 && id < 100_000)
}

#[test]
fn parses_fixed_and_preview_uses_range_minimum() {
    let policies = parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":2,"soulbound":true},{"type_id":12,"quantity_min":3,"quantity_max":7}]}"#).unwrap();
    assert_eq!(policies[&10].preview(None), vec![(11, 2), (12, 3)]);
    assert_eq!(policies[&10].summary(), Summary { entries: 2, bound_entries: 1, groups: 0, draws: 0 });
}

#[test]
fn fixed_all_rejects_probability_metadata() {
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":2,"probability":0.2},{"type_id":12,"quantity":1}]}"#).is_err());
}

#[test]
fn fixed_weighted_normalizes_and_previews_guaranteed_and_pool_entries() {
    let policy = parse(r#"{"type_id":10,"mode":"fixed_weighted","entries":[{"type_id":11,"quantity":2},{"type_id":12,"quantity_min":3,"quantity_max":7,"probability":0.4},{"type_id":13,"quantity":1,"probability":0.6}]}"#).unwrap().remove(&10).unwrap();
    assert_eq!(policy.preview(None), vec![(11, 2), (12, 3), (13, 1)]);
    assert_eq!(policy.summary(), Summary { entries: 3, bound_entries: 0, groups: 0, draws: 0 });
}

#[test]
fn race_policy_requires_all_starting_races_and_filters_preview() {
    let policy = parse(r#"{"type_id":10,"mode":"race_based","entries":[{"type_id":11,"quantity":1},{"type_id":12,"quantity":1,"race":"Amarr"},{"type_id":13,"quantity":1,"race":"Caldari"},{"type_id":14,"quantity":1,"race":"Gallente"},{"type_id":15,"quantity":1,"race":"Minmatar"}]}"#).unwrap().remove(&10).unwrap();
    assert_eq!(policy.preview(Some(RACE_AMARR)), vec![(11, 1), (12, 1)]);
}

#[test]
fn rejects_unknown_fields_duplicates_and_unvalidated_types() {
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1}],"extra":true}"#).is_err());
    assert!(parse(concat!(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1}]}"#, "\n", r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1}]}"#)).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":100000,"quantity":1}]}"#).is_err());
}

#[test]
fn rejects_partial_ranges_probability_and_mode_misuse() {
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity_min":1}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"random_weighted","draws":1,"entries":[{"type_id":11,"quantity":1,"probability":0.4},{"type_id":12,"quantity":1,"probability":0.4}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"random_multi","draws":1,"groups":[{"chance":1,"entries":[]}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1,"probability":-0.1}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1,"probability":1.1}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1,"probability":1e999}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1}],"draws":0}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[{"type_id":11,"quantity":1}],"groups":[]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_all","entries":[]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_weighted","entries":[{"type_id":11,"quantity":1,"probability":0.5},{"type_id":12,"quantity":1,"probability":0.5}]}"#).is_ok());
    assert!(parse(r#"{"type_id":10,"mode":"fixed_weighted","entries":[{"type_id":11,"quantity":1}]}"#).is_err());
}

#[test]
fn random_preview_excludes_zero_probability_and_chance() {
    let policy = parse(r#"{"type_id":10,"mode":"random_multi","draws":1,"groups":[{"chance":1,"entries":[{"type_id":11,"quantity":1,"probability":1.0}]},{"chance":0,"entries":[{"type_id":12,"quantity":1,"probability":1.0}]}]}"#).unwrap().remove(&10).unwrap();
    assert_eq!(policy.preview(None), vec![(11, 1)]);
}

#[test]
fn guaranteed_groups_are_exact_and_mutually_exclusive_candidates_are_not_summed() {
    assert!(parse(r#"{"type_id":10,"mode":"random_multi","draws":1,"groups":[{"chance":0.5,"entries":[{"type_id":11,"quantity":1,"probability":1}]}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"random_multi","draws":1,"groups":[{"chance":1,"entries":[{"type_id":11,"quantity":1,"probability":1}]},{"chance":1,"entries":[{"type_id":12,"quantity":1,"probability":1}]}]}"#).is_err());
    assert!(parse(r#"{"type_id":10,"mode":"random_weighted","draws":1,"entries":[{"type_id":11,"quantity":2147483647,"probability":0.5},{"type_id":12,"quantity":2147483647,"probability":0.5}]}"#).is_ok());
}
