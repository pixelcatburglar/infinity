use super::super::*;
use super::fixtures::*;
use super::fixtures::activate;
use crate::dao::skills::{queue as skill_queue, training as skill_rows};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn biology_does_not_change_persisted_real_booster_duration() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let biology = c.sde.types.iter().find(|t| t.name.as_deref() == Some("Biology") && t.category_id == Some(infinity_core::consts::CATEGORY_SKILL)).expect("Biology skill required").type_id;
    let ty = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)
        && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
        && t.attribute(ATTR_BOOSTER_DURATION).is_some_and(|v| v > 0.0 && v.is_finite())
        && t.attribute(ATTR_BOOSTER_MAX_CHAR_AGE_HOURS).is_none() && t.attribute(ATTR_BOOSTER_LAST_INJECTION_DATETIME).is_none()
        && !requirements(&c.sde, t.type_id).any(|(id, _)| id == biology)
        && !t.effects().iter().filter_map(|e| c.sde.dogma.effect(e.effect_id)).flat_map(|e| e.modifiers()).any(|m| m.modified == ATTR_BOOSTER_DURATION)).expect("booster without Biology prerequisite or duration modifier required").type_id;
    let base_duration = c.sde.types.attribute(ty, ATTR_BOOSTER_DURATION).expect("booster duration");
    for (skill, level) in requirements(&c.sde, ty) { skill_rows::starting_skill(&db, f.character, skill, level, 0).await.unwrap(); }
    let now = filetime_now();
    let mut durations = Vec::new();
    for level in [0, 5] {
        let rank = c.sde.types.attribute(biology, infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT).expect("Biology rank");
        let points = super::super::super::formulas::sp_for_level(rank, i64::from(level));
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4) ON CONFLICT (character_id,type_id) DO UPDATE SET level=EXCLUDED.level,skill_points=EXCLUDED.skill_points", &[&f.character, &biology, &level, &points]).await.unwrap();
        let source = item(&db, f.character, ty, STATION, 1).await;
        let draws = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.5)).collect();
        activate(&db, &c.sde, f.character, source, Some(STATION), now, true, draws).await.unwrap_or_else(|_| panic!("Biology fixture activation failed"));
        let metadata = brain::get(&db, f.character, source).await.unwrap().unwrap();
        let fit = Fit::load_character_at(&db, c.sde.clone(), f.character, now).await.unwrap_or_else(|_| panic!("Biology fit failed"));
        let ticks = metadata.expires_at.unwrap() - now;
        assert_eq!(ticks, filetime_ticks_from_ms(fit.value(Key::Booster(source), ATTR_BOOSTER_DURATION)));
        assert_eq!(ticks, filetime_ticks_from_ms(base_duration));
        durations.push(ticks);
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[&source]).await.unwrap();
        crate::scheduler::cancel(&db, &format!("{}:{}", completion::KIND, f.character)).await.unwrap();
    }
    assert_eq!(durations[1], durations[0], "Biology must not change persisted booster duration");
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn implant_mutation_banks_old_rate_and_retimes_once() {
    use infinity_core::consts::dogma::CHAR_ATTRIBUTES;
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let (ty, modifying, modified) = c.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)
        && t.attribute(ATTR_IMPLANTNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0))
        .find_map(|t| t.effects().iter().filter_map(|e| c.sde.dogma.effect(e.effect_id)).filter(|e| e.effect_category.unwrap_or(0) == 0).flat_map(|e| e.modifiers()).find(|m| m.func == "ItemModifier" && m.domain == "charID" && m.op == 2 && CHAR_ATTRIBUTES.contains(&(m.modified as i64)) && m.modified != m.modifying).map(|m| (t.type_id, m.modifying, m.modified))).expect("training attribute implant required");
    for (id, level) in requirements(&c.sde, ty) { skill_rows::starting_skill(&db, f.character, id, level, 0).await.unwrap(); }
    let primary = c.sde.dogma.attributes_by_name("primaryAttribute").next().expect("primary skill attribute").attribute_id;
    let secondary = c.sde.dogma.attributes_by_name("secondaryAttribute").next().expect("secondary skill attribute").attribute_id;
    let skill = c.sde.types.iter().find(|t| t.attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some() && [primary, secondary].into_iter().any(|a| t.attribute(a) == Some(f64::from(modified))) && !requirements(&c.sde, ty).any(|(id, _)| id == t.type_id)).expect("skill using modified training attribute required").type_id;
    skill_rows::starting_skill(&db, f.character, skill, 0, 0).await.unwrap();
    let now = filetime_now();
    let start = now - infinity_core::consts::FILETIME_MINUTE;
    let attrs = character_attributes_at(&db, &c.sde, f.character, start).await.unwrap_or_else(|_| panic!("character attributes"));
    let mut row = skill_queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 100_000, start_time: None, end_time: None };
    let end = training::schedule(&c.sde, &mut row, &attrs, start).unwrap_or_else(|_| panic!("scheduling training"));
    assert!(end > now);
    skill_queue::insert(&db, f.character, &row).await.unwrap();
    let source = item(&db, f.character, ty, STATION, 1).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&source, &modifying]).await.unwrap();
    let (_, effects) = activate(&db, &c.sde, f.character, source, None, now, false, vec![]).await.unwrap_or_else(|_| panic!("training implant activation failed"));
    let expected = training::points_at(0, row.dest_sp, start, end, now);
    let current = skill_rows::current(&db, f.character).await.unwrap();
    assert_eq!(current.iter().find(|s| s.type_id == skill).unwrap().skill_points, expected);
    let after = skill_queue::list(&db, f.character).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].end_time, row.end_time);
    assert_eq!(notices(&effects, crate::client::BRAIN_REFRESH), 1);
    assert_eq!(notices(&effects, "OnNewSkillQueueSaved"), 1);
    assert_eq!(notices(&effects, "OnServerImplantsChanged"), 1);
    let later = now + infinity_core::consts::FILETIME_MINUTE;
    let old = after[0].clone();
    let (_, removal) = destroy(&db, &c.sde, f.character, source, later).await.unwrap_or_else(|_| panic!("training implant removal failed"));
    let banked = training::points_at(old.start_sp, old.dest_sp, old.start_time.unwrap(), old.end_time.unwrap(), later);
    let current = skill_rows::current(&db, f.character).await.unwrap();
    assert_eq!(current.iter().find(|s| s.type_id == skill).unwrap().skill_points, banked);
    assert_eq!(notices(&removal, crate::client::BRAIN_REFRESH), 1);
    assert_eq!(notices(&removal, "OnNewSkillQueueSaved"), 1);
    assert_eq!(notices(&removal, "OnServerImplantsChanged"), 1);
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn booster_activation_banks_and_notifies_training_once() {
    use infinity_core::consts::dogma::CHAR_ATTRIBUTES;
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let (ty, modifying, modified) = c.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)
        && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
        && t.attribute(ATTR_BOOSTER_DURATION).is_some_and(|v| v > 0.0 && v.is_finite())
        && t.attribute(ATTR_BOOSTER_MAX_CHAR_AGE_HOURS).is_none() && t.attribute(ATTR_BOOSTER_LAST_INJECTION_DATETIME).is_none())
        .find_map(|t| t.effects().iter().filter_map(|e| c.sde.dogma.effect(e.effect_id)).flat_map(|e| e.modifiers()).find(|m| CHAR_ATTRIBUTES.contains(&(m.modified as i64)) && m.modified != m.modifying).map(|m| (t.type_id, m.modifying, m.modified))).expect("training attribute booster required");
    for (id, level) in requirements(&c.sde, ty) { skill_rows::starting_skill(&db, f.character, id, level, 0).await.unwrap(); }
    let primary = c.sde.dogma.attributes_by_name("primaryAttribute").next().expect("primary skill attribute").attribute_id;
    let secondary = c.sde.dogma.attributes_by_name("secondaryAttribute").next().expect("secondary skill attribute").attribute_id;
    let skill = c.sde.types.iter().find(|t| t.attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some() && [primary, secondary].into_iter().any(|a| t.attribute(a) == Some(f64::from(modified))) && !requirements(&c.sde, ty).any(|(id, _)| id == t.type_id)).expect("skill using booster attribute required").type_id;
    skill_rows::starting_skill(&db, f.character, skill, 0, 0).await.unwrap();
    let now = filetime_now();
    let start = now - infinity_core::consts::FILETIME_MINUTE;
    let attrs = character_attributes_at(&db, &c.sde, f.character, start).await.unwrap_or_else(|_| panic!("character attributes"));
    let mut row = skill_queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 100_000, start_time: None, end_time: None };
    let end = training::schedule(&c.sde, &mut row, &attrs, start).unwrap_or_else(|_| panic!("scheduling training"));
    assert!(end > now);
    skill_queue::insert(&db, f.character, &row).await.unwrap();
    let source = item(&db, f.character, ty, STATION, 1).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&source, &modifying]).await.unwrap();
    let draws = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.0)).collect();
    let (_, effects) = activate(&db, &c.sde, f.character, source, Some(STATION), now, true, draws).await.unwrap_or_else(|_| panic!("training booster activation failed"));
    let expected = training::points_at(0, row.dest_sp, start, end, now);
    assert_eq!(skill_rows::current(&db, f.character).await.unwrap().iter().find(|s| s.type_id == skill).unwrap().skill_points, expected);
    let after = skill_queue::list(&db, f.character).await.unwrap();
    assert_ne!(after[0].end_time, row.end_time);
    assert_eq!(notices(&effects, crate::client::BRAIN_REFRESH), 1);
    assert_eq!(notices(&effects, "OnNewSkillQueueSaved"), 1);
    assert_eq!(notices(&effects, "OnServerBoostersChanged"), 1);
    cleanup(&db, f).await;
}
