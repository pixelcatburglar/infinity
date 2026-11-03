//! Body transition training and notification regressions.
use super::*;
use crate::{client::filetime_now, consts::{dogma::{ATTR_IMPLANTNESS, CHAR_ATTRIBUTES}, CATEGORY_IMPLANT, FLAG_IMPLANT, GROUP_BOOSTER, FILETIME_MINUTE}, dao::{items::transfers, skills::{queue, training as skill_rows}}, db::outbox::{Effect, Notification}, services::tests::{client, fixture}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn transition_preserves_unchanged_timeline_and_banks_old_rate_on_change() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let (implant, modifying, modified, slot) = c.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER))
        .filter_map(|t| t.attribute(ATTR_IMPLANTNESS).filter(|v| *v > 0.0 && v.fract() == 0.0).map(|slot| (t, slot as i32)))
        .find_map(|(t, slot)| t.effects().iter().filter_map(|e| c.sde.dogma.effect(e.effect_id)).filter(|e| e.effect_category.unwrap_or(0) == 0).flat_map(|e| e.modifiers()).find(|m| m.func == "ItemModifier" && m.domain == "charID" && m.op == 2 && CHAR_ATTRIBUTES.contains(&(m.modified as i64)) && m.modified != m.modifying).map(|m| (t.type_id, m.modifying, m.modified, slot))).expect("training attribute implant");
    let primary = c.sde.dogma.attributes_by_name("primaryAttribute").next().unwrap().attribute_id;
    let secondary = c.sde.dogma.attributes_by_name("secondaryAttribute").next().unwrap().attribute_id;
    let skill = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some() && [primary, secondary].into_iter().any(|a| t.attribute(a) == Some(f64::from(modified)))).unwrap().type_id;
    skill_rows::starting_skill(&db, f.character, skill, 0, 0).await.unwrap();
    let now = filetime_now();
    let item = transfers::insert(&db, implant, f.character, f.character, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=$2 WHERE item_id=$1", &[&item, &slot]).await.unwrap();
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &modifying]).await.unwrap();
    let start = now - FILETIME_MINUTE;
    let attrs = character_attributes_at(&db, &c.sde, f.character, start).await.unwrap();
    let mut row = queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 100_000, start_time: None, end_time: None };
    let end = training::schedule(&c.sde, &mut row, &attrs, start).unwrap();
    assert!(end > now);
    queue::insert(&db, f.character, &row).await.unwrap();
    let owner = f.character;
    let _market = crate::services::market::lock().await;
    let effects = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let change = begin_tx(tx, outbox, &sde, owner, now).await?;
            finish_tx(tx, outbox, &sde, owner, now, change, false, false).await
        })
    }, |(), outbox| async move { Ok(outbox.into_entries()) }).await.unwrap();
    let unchanged = queue::list(&db, owner).await.unwrap();
    assert_eq!(unchanged[0].start_time, row.start_time);
    assert_eq!(unchanged[0].end_time, row.end_time);
    assert_eq!(unchanged[0].start_sp, row.start_sp);
    assert!(!effects.iter().any(|e| matches!(e, Effect::Notification(Notification::Pilot { event, .. }) if event == "OnNewSkillQueueSaved")));
    let effects = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let change = begin_tx(tx, outbox, &sde, owner, now).await?;
            tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.map_err(super::super::db_err)?;
            finish_tx(tx, outbox, &sde, owner, now, change, true, false).await
        })
    }, |(), outbox| async move { Ok(outbox.into_entries()) }).await.unwrap();
    let after = queue::list(&db, owner).await.unwrap();
    let banked = training::points_at(row.start_sp, row.dest_sp, start, end, now);
    assert_eq!(after[0].start_sp, banked);
    assert_eq!(skill_rows::current(&db, owner).await.unwrap().iter().find(|s| s.type_id == skill).unwrap().skill_points, banked);
    assert_ne!(after[0].end_time, row.end_time);
    for event in ["OnServerImplantsChanged", "OnNewSkillQueueSaved", crate::client::BRAIN_REFRESH] {
        assert_eq!(effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { event: name, .. }) if name == event)).count(), 1);
    }
    crate::scheduler::cancel(&db, &format!("{}:{}", completion::KIND, owner)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn booster_loss_refreshes_remaining_credits_and_preserves_expiry() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let ty = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)).expect("published booster").type_id;
    let owner = f.character;
    let now = filetime_now();
    let expiry = now + FILETIME_MINUTE;
    let mut ids = Vec::new();
    for (slot, uses) in [(1_i32, 2_f64), (2_i32, 3_f64)] {
        let id = transfers::insert(&db, ty, owner, owner, crate::consts::FLAG_BOOSTER, 1).await.unwrap();
        db.execute("UPDATE game.items SET singleton=true, brain_slot=$2, brain_activated_at=$3, brain_expires_at=$4 WHERE item_id=$1", &[&id, &slot, &now, &expiry]).await.unwrap();
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&id, &crate::consts::dogma::ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, &uses]).await.unwrap();
        ids.push(id);
    }
    let _market = crate::services::market::lock().await;
    let effects = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        let removed = ids[0];
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let change = begin_tx(tx, outbox, &sde, owner, now).await?;
            tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&removed]).await.map_err(super::super::db_err)?;
            finish_tx(tx, outbox, &sde, owner, now, change, false, true).await
        })
    }, |(), outbox| async move { Ok(outbox.into_entries()) }).await.unwrap();
    let notices: Vec<_> = effects.iter().filter_map(|effect| match effect {
        Effect::Notification(Notification::Pilot { event, args, .. }) => Some((event.as_str(), args.as_slice())),
        _ => None,
    }).collect();
    assert_eq!(notices, vec![
        ("OnNonDiminishingInjectionsRemoved", [].as_slice()),
        ("OnNonDiminishingInjectionsAdded", [Value::Int(3)].as_slice()),
        (crate::client::BRAIN_REFRESH, [].as_slice()),
        ("OnServerBoostersChanged", [].as_slice()),
    ]);
    let surviving = crate::dao::items::brain::get(&db, owner, ids[1]).await.unwrap().unwrap();
    assert_eq!(surviving.expires_at, Some(expiry));
    crate::scheduler::cancel(&db, &format!("{}:{}", completion::KIND, owner)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn projected_capacity_changes_at_completion_without_persisting() {
    use crate::{consts::{dogma::ATTR_MAX_JUMP_CLONES, SKILL_INFOMORPH_PSYCHOLOGY}, services::fit::Key};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let owner = f.character;
    let skill = SKILL_INFOMORPH_PSYCHOLOGY;
    skill_rows::set_level(&db, owner, skill, 0, 0).await.unwrap();
    let end = filetime_now();
    let points = super::super::formulas::sp_for_level(super::super::training::skill_type(&c.sde, skill).rank, 1);
    let row = queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: points, start_time: Some(end - FILETIME_MINUTE), end_time: Some(end) };
    queue::insert(&db, owner, &row).await.unwrap();
    let _market = crate::services::market::lock().await;
    let (before, at, after, unchanged) = db.transaction(|tx, _outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let skills_before = skill_rows::current(tx, owner).await.map_err(super::super::db_err)?;
            let queue_before = queue::list(tx, owner).await.map_err(super::super::db_err)?;
            let before = projected_fit_tx(tx, &sde, owner, end - 1).await?.value(Key::Char, ATTR_MAX_JUMP_CLONES);
            let at = projected_fit_tx(tx, &sde, owner, end).await?.value(Key::Char, ATTR_MAX_JUMP_CLONES);
            let after = projected_fit_tx(tx, &sde, owner, end + 1).await?.value(Key::Char, ATTR_MAX_JUMP_CLONES);
            let unchanged = skills_before == skill_rows::current(tx, owner).await.map_err(super::super::db_err)?
                && queue_before == queue::list(tx, owner).await.map_err(super::super::db_err)?;
            Ok::<_, CallError>((before, at, after, unchanged))
        })
    }, |value, _outbox| async move { Ok(value) }).await.unwrap();
    assert!(at > before);
    assert_eq!(after, at);
    assert!(unchanged);
    fixture::cleanup(&db, f).await.unwrap();
}

/// characterskills/util.py GetSkillPointsPerMinute and
/// SkillTrainingTimeCalculator._get_training_time_given_skill_points_to_train.
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn training_schedule_matches_client_duration_cases() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let primary_attr = c.sde.dogma.attributes_by_name("primaryAttribute").next().unwrap().attribute_id;
    let secondary_attr = c.sde.dogma.attributes_by_name("secondaryAttribute").next().unwrap().attribute_id;
    let (skill, primary, secondary) = c.sde.types.iter().find_map(|ty| {
        let primary = ty.attribute(primary_attr)? as i64;
        let secondary = ty.attribute(secondary_attr)? as i64;
        (ty.category_id == Some(crate::consts::CATEGORY_SKILL) && primary != secondary).then_some((ty.type_id, primary, secondary))
    }).expect("skill with distinct training attributes");
    for (primary_value, secondary_value, remaining, expected) in [
        (20.0, 20.0, 1000, 20_000_000_000),
        (23.0, 23.0, 1000, 17_391_304_347),
        (25.0, 25.0, 1000, 16_000_000_000),
        (20.1, 20.2, 1000, 19_867_549_668),
        (17.0, 23.0, 1, 21_052_631),
        (20.0, 20.0, 0, 0),
    ] {
        let attrs = [(primary, primary_value), (secondary, secondary_value)].into_iter().collect();
        let mut row = queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: remaining, start_time: None, end_time: None };
        assert_eq!(training::schedule(&c.sde, &mut row, &attrs, 0).unwrap(), expected);
        assert_eq!(row.end_time, Some(expected));
    }
    fixture::cleanup(&db, f).await.unwrap();
}
