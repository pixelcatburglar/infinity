use super::*;
use serde_json::{json, Value};

fn row(key: &str, data: Value) -> NativeRow {
    NativeRow { key: key.to_owned(), data }
}

fn parse_test(rows: Vec<NativeRow>) -> NativeCatalogue {
    parse_validated(rows, |id| (1..=6).contains(&id))
}

#[test]
fn equivalent_rows_are_canonical_and_keep_provenance() {
    let a = json!({"typeID":1,"_key":10,"nameID":20,"fixedLootItems":[3,2],"fixedCustomLootItems":[{"loottypeid":4,"lootcount":2,"lootsoulbound":1}]});
    let b = json!({"typeID":1,"_key":11,"descriptionID":21,"fixedLootItems":[2,3],"fixedCustomLootItems":[{"loottypeid":4,"lootcount":2,"lootsoulbound":1}]});
    let result = parse_test(vec![row("z", a), row("a", b)]);
    let d = &result.definitions[0];
    assert_eq!(d.source_keys, ["a", "z"]);
    assert!(!d.ambiguous && !d.malformed);
    assert_eq!(d.ordinary.iter().map(|r| r.type_id).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(d.custom, [NativeReward { type_id: 4, quantity: 2, soulbound: true }]);
}

#[test]
fn conflicts_never_choose_a_winner() {
    let result = parse_test(vec![row("a", json!({"typeID":1,"fixedLootItems":[2]})), row("b", json!({"typeID":1,"fixedLootItems":[3]}))]);
    let d = &result.definitions[0];
    assert!(d.ambiguous);
    assert_eq!(d.source_keys, ["a", "b"]);
    assert!(d.ordinary.is_empty() && d.custom.is_empty());
}

#[test]
fn native_rewards_validate_binding_count_and_existence_atomically() {
    for bound in [json!(true), json!(1), json!(false), json!(0)] {
        let d = parse_test(vec![row("a", json!({"typeID":1,"fixedLootItems":[2],"fixedCustomLootItems":[{"loottypeid":3,"lootcount":i32::MAX,"lootsoulbound":bound}]}))]);
        assert!(!d.definitions[0].malformed);
        assert_eq!(d.definitions[0].custom[0].quantity, i64::from(i32::MAX));
    }
    for quantity in [2147483648_i64, 4294967296, 9007199254740993, i64::MAX] {
        let d = parse_test(vec![row("a", json!({"typeID":1,"fixedCustomLootItems":[{"loottypeid":3,"lootcount":quantity,"lootsoulbound":0}]}))]);
        assert_eq!(d.definitions[0].custom[0].quantity, quantity);
    }
    for item in [json!({"loottypeid":3,"lootcount":0,"lootsoulbound":0}), json!({"loottypeid":7,"lootcount":1,"lootsoulbound":0}), json!({"loottypeid":3,"lootcount":1,"lootsoulbound":2}), json!({"loottypeid":3,"lootcount":1})] {
        let result = parse_test(vec![row("a", json!({"typeID":1,"fixedLootItems":[2],"fixedCustomLootItems":[item]}))]);
        let d = &result.definitions[0];
        assert!(d.malformed && d.ordinary.is_empty() && d.custom.is_empty());
    }
    for data in [json!({"typeID":1,"fixedLootItems":[7]}), json!({"typeID":7,"fixedLootItems":[2]}), json!({"typeID":1,"fixedLootItems":[{"typeID":2}]})] {
        assert!(parse_test(vec![row("a", data)]).definitions[0].malformed);
    }
}

#[test]
fn missing_null_and_empty_remain_distinct() {
    for (a, b) in [(json!({}), json!({"fixedLootItems":null})), (json!({"fixedLootItems":null}), json!({"fixedLootItems":[]})), (json!({}), json!({"fixedLootItems":[]}))] {
        let mut a = a; let mut b = b;
        a["typeID"] = json!(1); b["typeID"] = json!(1);
        assert!(parse_test(vec![row("a", a), row("b", b)]).definitions[0].ambiguous);
    }
    let missing = parse_test(vec![row("a", json!({"typeID":1}))]);
    let null = parse_test(vec![row("a", json!({"typeID":1,"fixedLootItems":null}))]);
    let empty = parse_test(vec![row("a", json!({"typeID":1,"fixedLootItems":[]}))]);
    assert!(!missing.definitions[0].ordinary_present);
    assert!(null.definitions[0].ordinary_present && empty.definitions[0].ordinary_present);
    assert_eq!(missing.definitions[0].missing_fields, null.definitions[0].missing_fields + 1);
    assert_eq!(null.definitions[0].empty_fields, empty.definitions[0].empty_fields);
}

#[test]
fn references_are_not_rewards_and_empty_special_is_inactive() {
    let empty = parse_test(vec![row("a", json!({"typeID":1,"skillBundleID":null,"fittings":[],"lootTableByRace":{}}))]);
    assert!(!empty.definitions[0].special);
    assert_eq!(empty.definitions[0].reference_count, 0);
    let result = parse_test(vec![row("a", json!({"typeID":1,"skillBundleID":9,"fittings":[8],"lootTableByRace":{"one":10,"two":11,"three":12,"four":13}}))]);
    let d = &result.definitions[0];
    assert!(!d.malformed && d.special);
    assert_eq!(d.reference_count, 6);
    assert!(d.ordinary.is_empty() && d.custom.is_empty());
}

#[test]
fn untyped_rows_are_preserved() {
    let result = parse_test(vec![row("missing", json!({})), row("overflow", json!({"typeID":2147483648_i64})), row("valid", json!({"typeID":1}))]);
    assert_eq!(result.untyped_rows, ["missing", "overflow"]);
    assert_eq!(result.definitions[0].source_keys, ["valid"]);
}

#[test]
fn unknown_or_invalid_presentation_and_table_modes_are_malformed() {
    for (field, value) in [
        ("lootPresentationType", json!("unknown")),
        ("lootPresentationType", json!(1)),
        ("lootTableType", json!("unknown")),
        ("lootTableType", json!([])),
    ] {
        let mut data = json!({"typeID":1,"fixedLootItems":[2]});
        data[field] = value;
        let result = parse_test(vec![row("a", data)]);
        assert!(result.definitions[0].malformed);
        assert!(result.definitions[0].ordinary.is_empty());
    }
    for data in [
        json!({"typeID":1}),
        json!({"typeID":1,"lootPresentationType":null,"lootTableType":null}),
        json!({"typeID":1,"lootPresentationType":"fixed","lootTableType":"byType"}),
        json!({"typeID":1,"lootPresentationType":"hacking","lootTableType":"byCharacterRace"}),
    ] {
        assert!(!parse_test(vec![row("a", data)]).definitions[0].malformed);
    }
}

#[test]
fn ambiguous_candidates_aggregate_special_and_malformed_safety_flags() {
    let special = parse_test(vec![
        row("a", json!({"typeID":1,"fixedLootItems":[2]})),
        row("b", json!({"typeID":1,"fixedLootItems":[3],"skillBundleID":4,"fittings":[5]})),
    ]);
    let d = &special.definitions[0];
    assert!(d.ambiguous && d.special && !d.malformed);
    assert!(d.ordinary.is_empty() && d.custom.is_empty() && d.splash.is_empty());

    let malformed = parse_test(vec![
        row("a", json!({"typeID":1,"fixedLootItems":[2]})),
        row("b", json!({"typeID":1,"fixedLootItems":[3],"lootPresentationType":"invalid"})),
    ]);
    let d = &malformed.definitions[0];
    assert!(d.ambiguous && d.malformed);
    assert!(!d.special);
    assert!(d.ordinary.is_empty() && d.custom.is_empty());
}

#[test]
fn ambiguous_valid_reward_conflicts_do_not_promote_content() {
    let result = parse_test(vec![
        row("a", json!({"typeID":1,"fixedLootItems":[2]})),
        row("b", json!({"typeID":1,"fixedLootItems":[3]})),
    ]);
    let d = &result.definitions[0];
    assert!(d.ambiguous && !d.special && !d.malformed);
    assert!(d.ordinary.is_empty() && d.custom.is_empty() && d.splash.is_empty());
}
