use super::*;
use serde_json::json;

fn fixture() -> FamilyRows {
    let mut rows = FamilyRows::new();
    for family in super::super::families::FAMILIES { rows.insert(family.name.into(), vec![]); }
    rows.insert("types".into(),vec![json!({"_key":1,"groupID":0,"typeNameID":1})]);
    rows.insert("groups".into(),vec![json!({"_key":0,"categoryID":0,"groupNameID":2})]);
    rows.insert("categories".into(),vec![json!({"_key":0,"categoryNameID":3})]);
    rows.insert("marketgroups".into(),vec![json!({"_key":0,"nameID":4})]);
    let texts=(1..=4).map(|i| json!({"_key":i,"text":"text"})).collect::<Vec<_>>();
    rows.insert("localization_fsd_en_us".into(),texts.clone());
    rows.insert("localization_fsd_zh".into(),texts);
    rows
}
#[test]
fn inherited_blueprint_path_only() {
    let mut rows=fixture();
    rows.insert("blueprints".into(),vec![json!({"_key":1,"blueprintTypeID":1,"activities":{"manufacturing":{"products":[{"typeID":99}]}}})]);
    let old=rows.clone();
    let result=validate(&rows,&old).unwrap();
    assert_eq!(result.warnings.len(),1);
    rows.get_mut("blueprints").unwrap()[0]["activities"]["manufacturing"]["products"][0]["typeID"]=json!(98);
    assert!(validate(&rows,&old).is_err());
}
#[test]
fn cycles_ranges_and_optional_zero() {
    let mut rows=fixture();
    rows.get_mut("types").unwrap()[0]["marketGroupID"]=json!(0);
    assert!(validate(&rows,&rows).is_ok());
    rows.get_mut("marketgroups").unwrap()[0]["parentGroupID"]=json!(0);
    assert!(validate(&rows,&rows).unwrap_err().to_string().contains("cycle"));
    rows.get_mut("marketgroups").unwrap()[0].as_object_mut().unwrap().remove("parentGroupID");
    rows.get_mut("types").unwrap()[0]["groupID"]=json!(-1);
    assert!(validate(&rows,&rows).is_err());
}
#[test]
fn english_required_and_zh_absence_warns() {
    let mut rows=fixture();
    rows.get_mut("localization_fsd_zh").unwrap().remove(0);
    assert_eq!(validate(&rows,&rows).unwrap().warnings.len(),1);
    rows.get_mut("localization_fsd_en_us").unwrap().remove(0);
    assert!(validate(&rows,&rows).is_err());
}
#[test]
fn required_skill_level_is_checked() {
    let mut rows=fixture();
    rows.insert("requiredskillsfortypes".into(),vec![json!({"_key":1,"1":6})]);
    assert!(validate(&rows,&rows).is_err());
}
