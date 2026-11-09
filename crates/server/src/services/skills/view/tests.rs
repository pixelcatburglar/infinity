//! CharacterSkillEntry wire compatibility checks for real and virtual skill state.
use super::*;

fn assert_round_trip(entry: Value, args: &[Value], timestamp: i64) {
    let decoded = marshal::loads(&marshal::dumps(&entry)).expect("skill entry marshal round-trip");
    let Value::Reduce { data, list, dict } = decoded else { panic!("expected CharacterSkillEntry reduce") };
    assert!(list.is_empty());
    assert!(dict.is_empty());
    let parts = data.items().expect("reduce data tuple");
    assert_eq!(parts[0], Value::global("characterskills.common.character_skill_entry.CharacterSkillEntry"));
    assert_eq!(parts[1].items().expect("constructor arguments"), args);
    assert_eq!(parts[2].get("timeStamp"), Some(&Value::Int(timestamp)));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn character_skill_entry_preserves_full_and_incremental_virtual_markers() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let skill = sde.types.iter()
        .find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SKILL) && ty.published == Some(true))
        .expect("published skill in SDE");
    let rank = super::training::skill_type(&sde, skill.type_id).rank;
    let real = [Value::Int(i64::from(skill.type_id)), Value::Int(0), Value::Int(0), Value::Float(rank), Value::None];
    assert_round_trip(skill_info(&sde, skill.type_id, Some(0), Some(0), None, 40), &real, 40);
    let full_virtual = [Value::Int(i64::from(skill.type_id)), Value::None, Value::None, Value::Float(rank), Value::Int(3)];
    assert_round_trip(skill_info(&sde, skill.type_id, None, None, Some(3), 41), &full_virtual, 41);
    let incremental_virtual = [Value::Int(i64::from(skill.type_id)), Value::Int(-1), Value::Int(-1), Value::Float(rank), Value::Int(3)];
    assert_round_trip(incremental_skill_info(&sde, skill.type_id, None, None, Some(3), 42), &incremental_virtual, 42);
    let deletion = [Value::Int(i64::from(skill.type_id)), Value::Int(-1), Value::Int(-1), Value::Float(rank), Value::None];
    assert_round_trip(incremental_skill_info(&sde, skill.type_id, None, None, None, 43), &deletion, 43);
}
