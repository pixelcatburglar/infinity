//! Parser and reference-validation tests using invented dynamic item definitions.
use super::*;
use serde_json::{json, Value};

fn definition() -> Value {
    json!({"_key": 1, "attributeIDs": {
        "10": {"_key": 10, "min": 0.5, "max": 1.5, "highIsGood": 0, "canEnsureGood": 1},
        "11": {"min": 1, "max": 2, "highIsGood": 1},
        "12": {"min": 1, "max": 1}
    }, "inputOutputMapping": [
        {"applicableTypes": [2, 3], "resultingType": 4},
        {"applicableTypes": [5], "resultingType": 6}
    ], "inputItemTypes": [], "badMutatorSkillTypeID": 0, "goodMutatorSkillTypeID": 0,
        "inputItemMetaLevelSum": 0, "canEnsureGoodAttributeCount": 0, "renewQuantity": 1})
}

fn parse(value: &Value) -> Result<(i32, Mutation), Exclusion> {
    parse_row(Some("1"), &value.to_string())
}

#[test]
fn parses_full_mapping_and_ranges() {
    let (id, mutation) = parse(&definition()).unwrap();
    assert_eq!(id, 1);
    assert_eq!(mutation.result_type(2), Some(4));
    assert_eq!(mutation.result_type(5), Some(6));
    assert_eq!(mutation.result_type(6), None);
    assert_eq!(mutation.attributes.keys().copied().collect::<Vec<_>>(), vec![10, 11, 12]);
    assert_eq!((mutation.attributes[&10].min, mutation.attributes[&10].max), (0.5, 1.5));
}

#[test]
fn validates_optional_polarity_without_storing_it() {
    let mut absent = definition();
    absent["attributeIDs"]["10"].as_object_mut().unwrap().remove("highIsGood");
    assert!(parse(&absent).is_ok());
    for value in [json!(false), json!(true), json!(0), json!(1)] {
        let mut v = definition();
        v["attributeIDs"]["10"]["highIsGood"] = value;
        let mutation = parse(&v).unwrap().1;
        assert_eq!((mutation.attributes[&10].min, mutation.attributes[&10].max), (0.5, 1.5));
    }
    for value in [json!(null), json!(-1), json!(2), json!(1.0), json!("true"), json!([]), json!({})] {
        let mut v = definition();
        v["attributeIDs"]["10"]["highIsGood"] = value;
        assert!(matches!(parse(&v), Err(Exclusion::Malformed)), "{v}");
    }
}

#[test]
fn rejects_malformed_ranges_and_mappings() {
    let mut cases = Vec::new();
    let mut v = definition(); v["attributeIDs"]["10"]["min"] = json!(0); cases.push(v);
    let mut v = definition(); v["attributeIDs"]["10"]["max"] = json!("2"); cases.push(v);
    let mut v = definition(); v["attributeIDs"]["10"]["highIsGood"] = json!(2); cases.push(v);
    let mut v = definition(); v["attributeIDs"] = json!({}); cases.push(v);
    let mut v = definition(); v["attributeIDs"]["010"] = json!({"min": 1, "max": 2}); cases.push(v);
    let mut v = definition(); v["inputOutputMapping"] = json!([]); cases.push(v);
    let mut v = definition(); v["inputOutputMapping"][0]["applicableTypes"] = json!([]); cases.push(v);
    let mut v = definition(); v["inputOutputMapping"][1]["applicableTypes"] = json!([2]); cases.push(v);
    let mut v = definition(); v["inputOutputMapping"][0]["resultingType"] = json!(0); cases.push(v);
    let mut v = definition(); v["inputOutputMapping"][0]["applicableTypes"] = json!(["2"]); cases.push(v);
    for value in cases { assert!(matches!(parse(&value), Err(Exclusion::Malformed)), "{value}"); }
    assert!(matches!(parse_row(Some("2"), &definition().to_string()), Err(Exclusion::Malformed)));
    assert!(matches!(parse_row(Some("1"), "{\"_key\":1,\"attributeIDs\":{\"10\":{\"min\":1e999,\"max\":1}},\"inputOutputMapping\":[]}"), Err(Exclusion::Malformed)));
}

#[test]
fn excludes_extra_inputs_and_unknown_references() {
    for (field, value) in [("inputItemTypes", json!([7])), ("badMutatorSkillTypeID", json!(8)),
        ("goodMutatorSkillTypeID", json!(8)), ("inputItemMetaLevelSum", json!(1)),
        ("canEnsureGoodAttributeCount", json!(1))] {
        let mut v = definition(); v[field] = value;
        assert!(matches!(parse(&v), Err(Exclusion::ExtraInput)));
    }
    let row = || DynamicItemRow { key: Some("1".into()), data: definition().to_string() };
    let valid = DynamicItems::index(vec![row()], |_| true, |_| true);
    assert!(valid.get(1).is_some());
    let missing_type = DynamicItems::index(vec![row()], |id| id != 4, |_| true);
    assert!(missing_type.get(1).is_none());
    let missing_attribute = DynamicItems::index(vec![row()], |_| true, |id| id != 10);
    assert!(missing_attribute.get(1).is_none());
    let duplicate = DynamicItems::index(vec![row(), row()], |_| true, |_| true);
    assert!(duplicate.get(1).is_none());
    let mut reversed = definition();
    reversed["attributeIDs"]["10"]["min"] = json!(2);
    reversed["attributeIDs"]["10"]["max"] = json!(1.5);
    let reversed_row = DynamicItemRow { key: Some("1".into()), data: reversed.to_string() };
    let indexed = DynamicItems::index(vec![reversed_row], |_| true, |_| true);
    let mutation = indexed.get(1).expect("reversed endpoints stay indexed");
    assert_eq!(mutation.result_type(2), Some(4));
    assert_eq!((mutation.attributes[&10].min, mutation.attributes[&10].max), (2.0, 1.5));
}
