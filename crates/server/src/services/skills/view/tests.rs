//! CharacterSkillEntry wire compatibility checks for real skill state.
use super::*;

fn assert_round_trip(
    entry: Value,
    type_id: i32,
    level: i32,
    points: i64,
    rank: f64,
    timestamp: i64,
) {
    let decoded = marshal::loads(&marshal::dumps(&entry)).expect("skill entry marshal round-trip");
    let Value::Reduce { data, list, dict } = decoded else { panic!("expected CharacterSkillEntry reduce") };
    assert!(list.is_empty());
    assert!(dict.is_empty());
    let parts = data.items().expect("reduce data tuple");
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], Value::global("characterskills.common.character_skill_entry.CharacterSkillEntry"));
    let args = parts[1].items().expect("constructor arguments");
    assert_eq!(args, [
        Value::Int(i64::from(type_id)),
        Value::Int(i64::from(level)),
        Value::Int(points),
        Value::Float(rank),
        Value::None,
    ]);
    let state = &parts[2];
    assert_eq!(state.get("timeStamp"), Some(&Value::Int(timestamp)));
    assert!(state.get("skillPoints").is_none());
    assert!(state.get("skillLevel").is_none());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn real_skill_entry_round_trips_zero_positive_and_deletion_states() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let skill = sde.types.iter()
        .find(|ty| ty.category_id == Some(crate::consts::CATEGORY_SKILL) && ty.published == Some(true))
        .expect("published skill in SDE");
    let rank = super::training::skill_type(&sde, skill.type_id).rank;

    assert_round_trip(skill_info(&sde, skill.type_id, 0, 0, 0), skill.type_id, 0, 0, rank, 0);
    assert_round_trip(skill_info(&sde, skill.type_id, 3, 12_345, 42), skill.type_id, 3, 12_345, rank, 42);
    assert_round_trip(skill_info(&sde, skill.type_id, -1, -1, 43), skill.type_id, -1, -1, rank, 43);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn skill_query_snapshot_preserves_real_rows_and_active_training() {
    use crate::services::tests::{client, fixture};
    use crate::services::fit::{Fit, Key};
    use crate::consts::dogma::{ATTR_SKILL_LEVEL, ATTR_SKILL_POINTS};

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = client(db.clone(), fixture.character).await;
    let mut types: Vec<_> = client.sde.types.iter()
        .filter(|ty| ty.category_id == Some(crate::consts::CATEGORY_SKILL) && ty.published == Some(true))
        .map(|ty| ty.type_id).collect();
    types.sort_unstable();
    assert!(types.len() >= 4, "SDE must contain four published skills");
    let (missing, zero, partial, active) = (types[0], types[1], types[2], types[3]);
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0),($1,$3,1,300),($1,$4,0,100)",
        &[&fixture.character, &zero, &partial, &active]).await.unwrap();
    let start = filetime_now() - crate::consts::FILETIME_MINUTE;
    let end = start + 2 * crate::consts::FILETIME_MINUTE;
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,100,200,$3,$4)",
        &[&fixture.character, &active, &start, &end]).await.unwrap();

    let wire = skills_dict(&mut client, fixture.character).await.unwrap_or_else(|_| panic!("skill query failed"));
    let decoded = marshal::loads(&marshal::dumps(&wire)).expect("skill dictionary marshal round-trip");
    let Value::Dict(entries) = decoded else { panic!("expected skill dictionary") };
    assert_eq!(entries.len(), 3);
    assert!(!entries.iter().any(|(key, _)| *key == Value::Int(i64::from(missing))));
    let entry = |id: i32| &entries.iter().find(|(key, _)| *key == Value::Int(i64::from(id))).expect("skill entry").1;
    let Value::Reduce { data, .. } = entry(zero) else { panic!("expected skill reduce") };
    let timestamp = data.items().unwrap()[2].get("timeStamp").and_then(Value::as_int).expect("timestamp");
    assert!(start < timestamp && timestamp < end);
    let trained = super::training::in_training_sp(&db, fixture.character, timestamp).await.unwrap_or_else(|_| panic!("active training query failed")).expect("active queue");
    assert_eq!(trained.0, active);
    assert!(trained.1 > 100 && trained.1 < 200);
    let rows = crate::dao::skills::training::current(&db, fixture.character).await.unwrap();
    let mut snapshot = Vec::new();
    for row in &rows {
        let points = if row.type_id == active { row.skill_points.max(trained.1) } else { row.skill_points };
        let rank = super::training::skill_type(&client.sde, row.type_id).rank;
        assert_round_trip(entry(row.type_id).clone(), row.type_id, row.level, points, rank, timestamp);
        snapshot.push(serde_json::json!({"type_id": row.type_id, "trained_level": row.level, "points": points, "rank": rank, "timestamp": timestamp}));
    }
    snapshot.sort_by_key(|row| row["type_id"].as_i64().unwrap());
    let fit = Fit::load_character_at(&db, client.sde.clone(), fixture.character, timestamp).await.unwrap_or_else(|_| panic!("character fit failed"));
    let fit_snapshot: Vec<_> = [zero, partial, active].into_iter().map(|id| serde_json::json!({
        "type_id": id,
        "skill_level": fit.value(Key::Skill(id), ATTR_SKILL_LEVEL),
        "skill_points": fit.value(Key::Skill(id), ATTR_SKILL_POINTS),
    })).collect();
    println!("{}", serde_json::json!({"missing_type_id": missing, "skills": snapshot, "fit": fit_snapshot}));
    fixture::cleanup(&db, fixture).await.unwrap();
}
