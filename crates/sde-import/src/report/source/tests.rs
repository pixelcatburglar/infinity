use super::*;
use crate::report::diff::parse_json;

fn changes(family: &str, old: &str, new: &str) -> Vec<Difference> {
    compare(family, ReportKey::integer("1").unwrap(), Some(&parse_json(old).unwrap()), Some(&parse_json(new).unwrap()), "source", "sha").unwrap()
}

#[test]
fn ieee_expansion_roundtrips_and_has_exact_values() {
    for value in [0.0, -0.0, 0.1, f64::from(f32::from_bits(0x3dcccccd)), f64::from_bits(1), f64::MIN_POSITIVE, f64::MAX, -f64::MAX] {
        let exact = ieee_decimal(value).unwrap();
        assert_eq!(exact.finite_f64().unwrap().to_bits(), value.to_bits(), "{value:?}");
    }
    assert_eq!(ieee_decimal(0.1).unwrap(), ExactNumber::parse("0.1000000000000000055511151231257827021181583404541015625").unwrap());
    assert_eq!(ieee_decimal(f64::from(0.1f32)).unwrap(), ExactNumber::parse("0.100000001490116119384765625").unwrap());
    assert_ne!(ieee_decimal(f64::from_bits(1)).unwrap(), ExactNumber::parse("5e-324").unwrap());
    assert!(ieee_decimal(f64::INFINITY).is_err());
}

#[test]
fn policy_a_preserves_snapshot_integer_and_converts_fraction() {
    assert!(changes("types", r#"{"mass":0.1}"#, r#"{"mass":0.1}"#).is_empty());
    let d = changes("types", r#"{"mass":100000000000000000000000000000000000}"#, r#"{"mass":1e35}"#);
    assert_eq!(d.len(), 1); assert_eq!(d[0].numeric_classification, "f64-representation-only");
    let d = changes("types", r#"{"mass":9007199254740993}"#, r#"{"mass":9007199254740992}"#);
    assert_eq!(d.len(), 1); assert_eq!(d[0].numeric_classification, "f64-representation-only");
    assert_eq!(changes("types", r#"{"mass":2}"#, r#"{"mass":3}"#)[0].numeric_classification, "exact-change");
    assert!(changes("types", r#"{"mass":1}"#, r#"{"mass":1.0}"#).is_empty());
    assert!(changes("types", r#"{"mass":1.0}"#, r#"{"mass":1e0}"#).is_empty());
}

#[test]
fn provenance_paths_cover_promoted_f32_nested_arrays_and_optional_values() {
    assert!(changes("metagroups", r#"{"color":[0.10000000149011612,1]}"#, r#"{"color":[0.10000000149011612,1.0]}"#).is_empty());
    let d = changes("metagroups", r#"{"color":[0.1]}"#, r#"{"color":[0.10000000149011612]}"#);
    assert_eq!(d[0].path, "/color/0"); assert_eq!(d[0].numeric_classification, "exact-change");
    assert!(changes("dogmaattributes", r#"{"defaultValue":0.10000000149011612}"#, r#"{"defaultValue":0.10000000149011612}"#).is_empty());
    assert!(changes("typedogma", r#"{"dogmaAttributes":[{"value":0.1}]}"#, r#"{"dogmaAttributes":[{"value":0.1}]}"#).is_empty());
    assert_eq!(changes("types", "{}", r#"{"radius":0.1}"#)[0].path, "/radius");
    assert!(changes("blueprints", r#"{"mass":0.1,"color":[0.1]}"#, r#"{"mass":0.10,"color":[1e-1]}"#).is_empty());
    assert_eq!(changes("blueprints", r#"{"mass":9007199254740993}"#, r#"{"mass":9007199254740992}"#).len(), 1);
}

#[test]
fn signed_zero_is_a_source_bit_change_only_at_binary_paths() {
    for (old, new, old_token, new_token) in [
        (r#"{"mass":-0.0}"#, r#"{"mass":0.0}"#, "-0", "0"),
        (r#"{"mass":0}"#, r#"{"mass":-0.0}"#, "0", "-0"),
    ] {
        let d = changes("types", old, new);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, "/mass");
        assert_eq!(d[0].numeric_classification, "exact-change");
        assert_eq!(d[0].source_relative_path, "source");
        assert_eq!(d[0].source_sha256, "sha");
        assert_eq!(serde_json::to_string(d[0].old_value.as_ref().unwrap()).unwrap(), old_token);
        assert_eq!(serde_json::to_string(d[0].new_value.as_ref().unwrap()).unwrap(), new_token);
    }
    let d = changes("typedogma", r#"{"dogmaAttributes":[{"value":-0.0}]}"#, r#"{"dogmaAttributes":[{"value":0.0}]}"#);
    assert_eq!(d.len(), 1); assert_eq!(d[0].path, "/dogmaAttributes/0/value");
    let d = changes("metagroups", r#"{"color":[0.0,-0.0]}"#, r#"{"color":[-0.0,0.0]}"#);
    assert_eq!(d.len(), 2); assert_eq!(d[0].path, "/color/0"); assert_eq!(d[1].path, "/color/1");
    assert!(d.iter().all(|d| d.numeric_classification == "exact-change"));
    assert_eq!(changes("types", "{}", r#"{"mass":-0.0}"#).len(), 1);
    assert!(changes("types", r#"{"mass":-0.0}"#, r#"{"mass":-0.0}"#).is_empty());
    assert!(changes("blueprints", r#"{"mass":-0.0}"#, r#"{"mass":0.0}"#).is_empty());
    let a = parse_json(r#"{"mass":-0.0}"#).unwrap();
    let b = parse_json(r#"{"mass":0.0}"#).unwrap();
    assert!(diff::compare("types", ReportKey::integer("1").unwrap(), Some(&a), Some(&b), "", "").is_empty());
}

#[test]
fn raw_decimal_comparison_remains_unchanged() {
    let a = parse_json(r#"{"mass":1}"#).unwrap();
    for token in [r#"{"mass":1.0}"#, r#"{"mass":1e0}"#] {
        let b = parse_json(token).unwrap();
        assert!(diff::compare("types", ReportKey::integer("1").unwrap(), Some(&a), Some(&b), "", "").is_empty());
    }
}

fn rows(entries: &[(i32, &str)]) -> BTreeMap<ReportKey, ExactValue> {
    entries.iter().map(|(key, json)| (ReportKey::integer(&key.to_string()).unwrap(), parse_json(json).unwrap())).collect()
}

#[test]
fn language_snapshot_projection_retains_other_differences_and_record_payloads() {
    for (family, language) in [("localization_fsd_en_us", "en-us"), ("localization_fsd_zh", "zh")] {
        let common = format!(r#"{{"_key":1,"text":"same","metadata":{{}},"tokens":[],"languageID":"{language}","extra":7}}"#);
        let removed = format!(r#"{{"_key":2,"text":"old","languageID":"{language}"}}"#);
        let mut old = rows(&[(2, &removed), (1, &common)]);
        let projection = project_snapshot_language(family, &mut old).unwrap().unwrap();
        assert_eq!(projection.expected_language, language);
        assert_eq!(projection.validated_rows, 2);
        let new = rows(&[(1, r#"{"_key":1,"text":"same","metadata":{},"tokens":[]}"#), (3, r#"{"_key":3,"text":"new"}"#)]);
        let keys: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
        let matching = parse_json(r#"{"_key":1,"text":"same","metadata":{},"tokens":[],"extra":7}"#).unwrap();
        assert!(compare(family, ReportKey::integer("1").unwrap(), old.get(&ReportKey::integer("1").unwrap()), Some(&matching), "", "").unwrap().is_empty());
        let differences: Vec<_> = keys.into_iter().flat_map(|key|
            compare(family, key.clone(), old.get(&key), new.get(&key), "client", "sha").unwrap()).collect();
        assert_eq!(differences.len(), 3);
        assert_eq!(differences.iter().map(|d| (&d.key, d.kind.as_str(), d.path.as_str())).collect::<Vec<_>>(),
            vec![(&ReportKey::integer("1").unwrap(), "changed", "/extra"),
                 (&ReportKey::integer("2").unwrap(), "sde-only", ""),
                 (&ReportKey::integer("3").unwrap(), "client-only", "")]);
        assert_eq!(differences[1].old_value.as_ref(), old.get(&ReportKey::integer("2").unwrap()));
        assert_eq!(differences[2].new_value.as_ref(), new.get(&ReportKey::integer("3").unwrap()));
        assert!(changes(family, r#"{"_key":1,"text":"same"}"#, r#"{"_key":1,"text":"same"}"#).is_empty());
    }
}

#[test]
fn language_projection_rejects_every_bad_row_without_partial_mutation() {
    for bad in [r#"{"languageID":"zh"}"#, r#"{}"#, r#"{"languageID":null}"#, r#"{"languageID":1}"#] {
        let mut old = rows(&[(1, r#"{"languageID":"en-us"}"#), (2, bad)]);
        let original = old.clone();
        let error = project_snapshot_language("localization_fsd_en_us", &mut old).err().unwrap();
        assert!(error.contains("localization_fsd_en_us") && error.contains("2") && error.contains("/languageID"));
        assert_eq!(old, original);
    }
}

#[test]
fn language_projection_is_family_scoped_and_handles_empty_snapshots() {
    let mut old = rows(&[(1, r#"{"languageID":null,"other":null}"#)]);
    let original = old.clone();
    assert!(project_snapshot_language("localization_fsd_main", &mut old).unwrap().is_none());
    assert_eq!(old, original);
    assert_eq!(compare("localization_fsd_main", ReportKey::integer("1").unwrap(), old.values().next(), Some(&parse_json("{}").unwrap()), "", "").unwrap().len(), 2);
    let mut empty = BTreeMap::new();
    let projection = project_snapshot_language("localization_fsd_zh", &mut empty).unwrap().unwrap();
    assert_eq!(projection.expected_language, "zh");
    assert_eq!(projection.validated_rows, 0);
}
