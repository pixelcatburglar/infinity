use super::*;
use serde_json::json;

fn fixture() -> FamilyRows {
    let mut rows = FamilyRows::new();
    rows.insert("localization_fsd_en_us".into(), vec![
        json!({"_key": 1, "text": "Synthetic name"}),
        json!({"_key": 2, "text": "Synthetic description"}),
        json!({"_key": 0, "text": "Zero message"}),
    ]);
    rows.insert("localization_fsd_zh".into(), vec![json!({"_key": 1, "text": "Synthetic translation"})]);
    rows.insert("types".into(), vec![json!({"_key": 101, "typeNameID": 1, "descriptionID": 2, "groupID": 7, "mass": 12.5, "volume": 3, "published": false, "iconID": 0})]);
    rows.insert("groups".into(), vec![json!({"_key": 7, "groupNameID": 1, "categoryID": 9})]);
    rows.insert("categories".into(), vec![json!({"_key": 9, "categoryNameID": 1})]);
    rows.insert("marketgroups".into(), vec![json!({"_key": 3, "nameID": 1, "descriptionID": 2})]);
    rows.insert("metagroups".into(), vec![json!({"_key": 4, "nameID": 1, "descriptionID": 2, "color": [0, 0.5, 1, 1]})]);
    rows.insert("dogmaattributes".into(), vec![json!({"_key": 5, "name": "attr", "categoryID": 0, "displayNameID": 0, "defaultValue": 2.5})]);
    rows.insert("dogmaeffects".into(), vec![json!({"_key": 6, "effectName": "effect", "effectCategory": 0, "modifierInfo": [{"operation": -1}, {"operation": -1}]})]);
    rows.insert("typedogma".into(), vec![json!({"_key": 101, "dogmaAttributes": [{"attributeID": 5, "value": 4.75}], "dogmaEffects": [{"effectID": 6, "isDefault": 0}, {"effectID": 6, "isDefault": 1}]})]);
    for family in ["dogmaattributecategories", "dogmaunits", "typematerials", "requiredskillsfortypes", "blueprints", "localization_fsd_main"] {
        rows.insert(family.into(), vec![json!({"_key": 11, "values": [0, null, false, 0]})]);
    }
    rows
}

#[test]
fn projects_same_source_without_changing_native_or_keysets() {
    let native = fixture();
    let original = native.clone();
    let compat = normalize(&native).unwrap();
    assert_eq!(native, original);
    assert_eq!(compat.len(), 8);
    for (family, rows) in &compat {
        assert_eq!(rows.iter().map(|r| &r["_key"]).collect::<Vec<_>>(), native[family].iter().map(|r| &r["_key"]).collect::<Vec<_>>());
    }
    assert!(!compat.contains_key("marketgroups"));
    assert!(!compat.contains_key("metagroups"));
    let ty = &compat["types"][0];
    assert_eq!(ty["_key"], 101);
    assert_eq!(ty["name"], "Synthetic name");
    assert_eq!(ty["description"], "Synthetic description");
    assert_eq!(ty["typeNameID"], 1);
    assert_eq!(ty["descriptionID"], 2);
    assert_eq!(ty["mass"], 12.5);
    assert_eq!(ty["volume"], 3);
    assert_eq!(ty["published"], false);
    assert_eq!(ty["iconID"], 0);
    assert_eq!(compat["groups"][0]["name"], "Synthetic name");
    assert_eq!(compat["categories"][0]["name"], "Synthetic name");
    assert_eq!(native["marketgroups"][0]["nameID"], 1);
    assert_eq!(compat["dogmaattributes"][0]["attributeCategoryID"], 0);
    assert_eq!(compat["dogmaattributes"][0]["displayName"], "Zero message");
    assert_eq!(compat["dogmaattributes"][0]["defaultValue"], 2.5);
    assert_eq!(compat["dogmaeffects"][0]["name"], "effect");
    assert_eq!(compat["dogmaeffects"][0]["effectCategoryID"], 0);
    assert_eq!(compat["dogmaeffects"][0]["modifierInfo"], native["dogmaeffects"][0]["modifierInfo"]);
    assert_eq!(compat["typedogma"], native["typedogma"]);
    assert_eq!(compat["typedogma"][0]["dogmaAttributes"][0]["value"], 4.75);
    assert_eq!(compat["blueprints"], native["blueprints"]);
}

#[test]
fn absent_null_and_zero_are_distinct() {
    let mut native = fixture();
    native.get_mut("types").unwrap().push(json!({"_key": 102, "typeNameID": 0, "descriptionID": null}));
    native.get_mut("types").unwrap().push(json!({"_key": 103, "typeNameID": 1}));
    native.get_mut("dogmaattributes").unwrap().push(json!({"_key": 12, "categoryID": null}));
    let compat = normalize(&native).unwrap();
    assert_eq!(compat["types"][1]["name"], "Zero message");
    assert!(compat["types"][1].get("description").unwrap().is_null());
    assert!(compat["types"][2].get("description").is_none());
    assert!(compat["dogmaattributes"][1]["attributeCategoryID"].is_null());
    assert!(compat["dogmaattributes"][1].get("displayName").is_none());
}

#[test]
fn missing_english_or_required_id_fails_without_translation_fallback() {
    let mut native = fixture();
    native.get_mut("localization_fsd_en_us").unwrap().retain(|row| row["_key"] != 1);
    assert!(normalize(&native).is_err());
    let mut native = fixture();
    native.get_mut("types").unwrap()[0].as_object_mut().unwrap().remove("typeNameID");
    assert!(normalize(&native).is_err());
    let mut native = fixture();
    native.get_mut("types").unwrap()[0]["descriptionID"] = json!(99);
    assert!(normalize(&native).is_err());
    let mut native = fixture();
    native.get_mut("marketgroups").unwrap()[0]["nameID"] = json!(99);
    assert!(normalize(&native).is_err());
}

#[test]
fn rejects_bad_or_duplicate_english_messages() {
    let mut native = fixture();
    native.get_mut("localization_fsd_en_us").unwrap().push(json!({"_key": 1, "text": "other"}));
    assert!(normalize(&native).is_err());
    let mut native = fixture();
    native.get_mut("localization_fsd_en_us").unwrap()[0]["text"] = json!(1);
    assert!(normalize(&native).is_err());
}
