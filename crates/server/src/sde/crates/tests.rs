use super::*;
use crate::dao::sde::crates::NativeRow;
use serde_json::json;

fn native(id: i32) -> native::NativeDefinition {
    native::NativeDefinition {
        type_id: id, source_keys: vec![format!("row-{id}")], splash: String::new(),
        ordinary_present: false, custom_present: false, ordinary: Vec::new(), custom: Vec::new(),
        special: false, ambiguous: false, malformed: false, reference_count: 0,
        missing_fields: 0, empty_fields: 0,
    }
}

fn reward(id: i32, quantity: i64) -> native::NativeReward {
    native::NativeReward { type_id: id, quantity, soulbound: false }
}

#[test]
fn native_only_definitions_and_refusals_remain_visible() {
    let mut choice = native(1);
    choice.custom = vec![reward(10, 2), reward(11, 3)];
    choice.custom_present = true;
    let empty = native(2);
    let mut choices = Vec::new();
    for id in 7..=9 {
        let mut definition = native(id);
        definition.custom_present = true;
        definition.custom.push(reward(10, 1));
        choices.push(definition);
    }
    let mut special = native(3);
    special.special = true;
    let mut ambiguous = native(4);
    ambiguous.ambiguous = true;
    let mut malformed = native(5);
    malformed.malformed = true;
    let unknown = native(6);
    let mut definitions = vec![choice, empty, special, ambiguous, malformed, unknown];
    definitions.extend(choices);
    let catalogue = Crates::compose(native::NativeCatalogue {
        definitions,
        untyped_rows: vec!["untyped".into()],
    }, BTreeMap::new(), |id| id != 6);
    assert_eq!(catalogue.iter().map(|d| d.type_id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let choice = catalogue.get(1).unwrap();
    assert_eq!(choice.preview(true, None), Ok(vec![(10, 2), (11, 3)]));
    assert_eq!(choice.preview(false, None), Ok(vec![]));
    assert!(!choice.sale_supported());
    assert_eq!(catalogue.get(2).unwrap().preview(false, None), Ok(vec![]));
    assert_eq!(catalogue.get(3).unwrap().preview(false, None), Err(Unavailable::Metadata));
    assert_eq!(catalogue.get(4).unwrap().preview(true, None), Err(Unavailable::Ambiguous));
    for id in 7..=9 {
        assert_eq!(catalogue.get(id).unwrap().preview(true, None), Ok(vec![(10, 1)]));
    }
    assert!(catalogue.get(12).is_none());
    assert_eq!(catalogue.get(5).unwrap().preview(false, None), Err(Unavailable::Malformed));
    assert_eq!(catalogue.get(6).unwrap().preview(false, None), Err(Unavailable::UnknownType));
}

// Uses actual type existence checks, while all crate rows and policy content are synthetic.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn parsed_sources_merge_without_overwriting_native_authority() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let types = Types::load(&db).await.unwrap();
    let actual_native = native::parse(crate::dao::sde::crates::load(&db).await.unwrap(), &types);
    let native_choices: Vec<_> = actual_native.definitions.iter()
        .filter(|d| !d.custom.is_empty() && !d.ambiguous && !d.malformed && !d.special)
        .map(|d| d.type_id).collect();
    let actual_catalogue = Crates::compose(actual_native, BTreeMap::new(), |id| types.get(id).is_some());
    assert!(native_choices.len() >= 4, "expected the native choice crates");
    for id in native_choices {
        assert!(!actual_catalogue.get(id).unwrap().preview(true, None).unwrap().is_empty());
    }
    let ids: Vec<i32> = types.iter().map(|t| t.type_id).filter(|id| *id > 0).take(12).collect();
    assert_eq!(ids.len(), 12);
    let row = |key: &str, data| NativeRow { key: key.into(), data };
    let rows = vec![
        row("choice-a", json!({"typeID":ids[0],"fixedCustomLootItems":[{"loottypeid":ids[8],"lootcount":2,"lootsoulbound":true}]})),
        row("choice-b", json!({"typeID":ids[0],"nameID":123,"fixedCustomLootItems":[{"loottypeid":ids[8],"lootcount":2,"lootsoulbound":true}]})),
        row("fixed", json!({"typeID":ids[1],"fixedLootItems":[ids[9]]})),
        row("empty", json!({"typeID":ids[2]})),
        row("special", json!({"typeID":ids[3],"skillBundleID":1})),
        row("conflict-a", json!({"typeID":ids[4],"fixedLootItems":[ids[8]]})),
        row("conflict-b", json!({"typeID":ids[4],"fixedLootItems":[ids[9]]})),
        row("native-only", json!({"typeID":ids[5],"fixedCustomLootItems":[{"loottypeid":ids[10],"lootcount":1,"lootsoulbound":false}]})),
    ];
    let mut policy_ids = vec![ids[0], ids[1], ids[2], ids[4], ids[6]];
    policy_ids.sort_unstable();
    let text = policy_ids.iter().map(|id| format!(r#"{{"type_id":{id},"mode":"fixed_all","entries":[{{"type_id":{},"quantity":4}}]}}"#, ids[11])).collect::<Vec<_>>().join("\n");
    let policies = policy::parse(&text, &types).unwrap();
    let catalogue = Crates::compose(native::parse(rows, &types), policies, |id| types.get(id).is_some());
    assert_eq!(catalogue.get(ids[0]).unwrap().preview(true, None), Ok(vec![(ids[8], 2)]));
    assert_eq!(catalogue.get(ids[0]).unwrap().preview(false, None), Ok(vec![]));
    assert_eq!(catalogue.get(ids[1]).unwrap().preview(false, None), Ok(vec![(ids[9], 1)]));
    assert_eq!(catalogue.get(ids[2]).unwrap().preview(false, None), Ok(vec![(ids[11], 4)]));
    assert_eq!(catalogue.get(ids[3]).unwrap().preview(false, None), Err(Unavailable::Metadata));
    assert_eq!(catalogue.get(ids[4]).unwrap().preview(false, None), Ok(vec![(ids[11], 4)]));
    assert_eq!(catalogue.get(ids[4]).unwrap().preview(true, None), Ok(vec![]));
    assert_eq!(catalogue.get(ids[5]).unwrap().preview(true, None), Ok(vec![(ids[10], 1)]));
    assert_eq!(catalogue.get(ids[6]).unwrap().preview(false, None), Ok(vec![(ids[11], 4)]));
    assert_eq!(catalogue.get(ids[6]).unwrap().preview(true, None), Ok(vec![]));
}
