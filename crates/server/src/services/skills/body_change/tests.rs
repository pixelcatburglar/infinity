//! Body transition training and notification regressions.
use super::*;
use infinity_core::{consts::{dogma::{ATTR_IMPLANTNESS, ATTR_NON_DESTRUCTIBLE, CHAR_ATTRIBUTES}, CATEGORY_IMPLANT, FLAG_IMPLANT, GROUP_BOOSTER, FILETIME_MINUTE}};
use crate::{client::filetime_now, dao::{items::{clones, dogma, transfers}, skills::{queue, training as skill_rows}}, db::outbox::{Effect, Notification}, services::tests::{client, fixture}};
use crate::services::fit::Key;

/// Insert an active implant row; `protection` writes the instance
/// non-destruction override. Returns the item id.
async fn implant_row(db: &crate::db::Db, owner: i64, kind: i32, slot: i32, protection: Option<f64>, expires_at: Option<i64>, effect_ids: Option<Vec<i32>>) -> i64 {
    let item = transfers::insert(db, kind, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=$2, brain_expires_at=$3, brain_effect_ids=$4 WHERE item_id=$1", &[&item, &slot, &expires_at, &effect_ids]).await.unwrap();
    if let Some(value) = protection {
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&item, &ATTR_NON_DESTRUCTIBLE, &value]).await.unwrap();
    }
    item
}

/// Park a new implant of `kind` under a dormant clone at `location`;
/// returns (clone id, item id).
async fn dormant_clone(db: &crate::db::Db, owner: i64, location: i64, kind: i32) -> (i64, i64) {
    let clone: i64 = db.query_one("INSERT INTO game.jump_clones (owner_id,location_id) VALUES ($1,$2) RETURNING clone_id", &[&owner, &location]).await.unwrap().get("clone_id");
    let item = transfers::insert(db, kind, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=1, jump_clone_id=$2 WHERE item_id=$1", &[&item, &clone]).await.unwrap();
    (clone, item)
}

/// Remove dormant clone custody left behind before the fixture cleanup.
async fn drop_clones(db: &crate::db::Db, owner: i64) {
    db.execute("DELETE FROM game.items WHERE owner_id=$1 AND jump_clone_id IS NOT NULL", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&owner]).await.unwrap();
}

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
    let skill = c.sde.types.iter().find(|t| t.attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some() && [primary, secondary].into_iter().any(|a| t.attribute(a) == Some(f64::from(modified)))).unwrap().type_id;
    skill_rows::starting_skill(&db, f.character, skill, 0, 0).await.unwrap();
    let now = filetime_now();
    let item = transfers::insert(&db, implant, f.character, f.character, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=$2 WHERE item_id=$1", &[&item, &slot]).await.unwrap();
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &modifying]).await.unwrap();
    let (clone, dormant) = dormant_clone(&db, f.character, c.sde.map.stations().next().unwrap().station_id, implant).await;
    let start = now - FILETIME_MINUTE;
    let attrs = character_attributes_at(&db, &c.sde, f.character, start).await.unwrap();
    let mut row = queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 100_000, start_time: None, end_time: None };
    let end = training::schedule(&c.sde, &mut row, &attrs, start).unwrap().unwrap();
    assert!(end > now);
    queue::insert(&db, f.character, &row).await.unwrap();
    let owner = f.character;
    let mut recipient = None;
    let scenario = crate::tests::catch_scenario(async {
    let owner_ship = crate::dao::items::creation::rookie_ship(&db, infinity_core::consts::TYPE_CAPSULE, owner, c.sde.map.stations().next().unwrap().station_id, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    crate::dao::characters::selection::set_ship(&db, owner, owner_ship).await.unwrap();
    recipient = Some({
        let market = crate::services::market::lock().await;
        fixture::LiveRecipient::cold(&c.world, owner, None, &market).await
    });
    let live = recipient.as_ref().unwrap();
    let world = &c.world;
    let fences = crate::services::effects::online::capture_online_fences(&c.world, &[owner]).await;
    let _market = crate::services::market::lock().await;
    let effects = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        let fences = fences.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let change = begin_tx(tx, outbox, &sde, owner, now).await?;
            finish_tx(tx, outbox, &sde, &fences, owner, now, change, false, false).await
        })
    }, |(), outbox| async move {
        Ok(fixture::collected_notifications(outbox, world))
    }).await.unwrap();
    let boosted = attrs[&i64::from(modified)];
    assert_eq!(live.read(move |fit|
        (fit.source_type(Key::Implant(item)), fit.value(Key::Char, modified))).await, (Some(implant), boosted));
    let unchanged = queue::list(&db, owner).await.unwrap();
    assert_eq!(unchanged[0].start_time, row.start_time);
    assert_eq!(unchanged[0].end_time, row.end_time);
    assert_eq!(unchanged[0].start_sp, row.start_sp);
    assert!(!effects.iter().any(|e| matches!(e, Effect::Notification(Notification::Pilot { event, .. }) if event == "OnNewSkillQueueSaved")));
    let (effects, refresh, destroyed) = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        let fences = fences.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let mut destroyed = Vec::new();
            let loss = begin_loss_tx(tx, outbox, &sde, owner, now, |item| destroyed.push(item.item_id)).await?;
            let refresh = finish_loss_tx(tx, outbox, &sde, &fences, owner, now, loss).await?;
            Ok::<_, CallError>((refresh, destroyed))
        })
    }, |(refresh, destroyed), outbox| async move {
        Ok((fixture::collected_notifications(outbox, world), refresh, destroyed))
    }).await.unwrap();
    assert_eq!(destroyed, [item]);
    assert!(refresh.implants_changed && !refresh.boosters_changed);
    assert_eq!(live.read(move |fit|
        (fit.source_type(Key::Implant(item)), fit.value(Key::Char, modified))).await, (None, boosted - 10.0));
    // Same-type dormant custody is never read, locked or deleted.
    assert_eq!(clones::list(&db, owner, clone).await.unwrap().iter().map(|i| i.item_id).collect::<Vec<_>>(), [dormant]);
    let after = queue::list(&db, owner).await.unwrap();
    let banked = training::points_at(row.start_sp, row.dest_sp, start, end, now);
    assert_eq!(after[0].start_sp, banked);
    assert_eq!(skill_rows::current(&db, owner).await.unwrap().iter().find(|s| s.type_id == skill).unwrap().skill_points, banked);
    assert_ne!(after[0].end_time, row.end_time);
    for event in ["OnServerImplantsChanged", "OnNewSkillQueueSaved", crate::client::BRAIN_REFRESH] {
        assert_eq!(effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { event: name, .. }) if name == event)).count(), 1);
    }
    }).await;
    if let Some(live) = recipient {
        live.close().await;
    }
    crate::scheduler::cancel(&db, &format!("{}:{}", completion::KIND, owner)).await.unwrap();
    drop_clones(&db, owner).await;
    fixture::cleanup(&db, f).await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn booster_loss_refreshes_remaining_credits_and_preserves_expiry() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let ty = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER) && !t.attribute(ATTR_NON_DESTRUCTIBLE).is_some_and(|v| v.is_finite() && v != 0.0)).expect("unprotected published booster").type_id;
    let owner = f.character;
    let now = filetime_now();
    let expiry = now + FILETIME_MINUTE;
    let mut ids = Vec::new();
    for (slot, uses) in [(1_i32, 2_f64), (2_i32, 3_f64)] {
        let id = transfers::insert(&db, ty, owner, owner, infinity_core::consts::FLAG_BOOSTER, 1).await.unwrap();
        db.execute("UPDATE game.items SET singleton=true, brain_slot=$2, brain_activated_at=$3, brain_expires_at=$4 WHERE item_id=$1", &[&id, &slot, &now, &expiry]).await.unwrap();
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&id, &infinity_core::consts::dogma::ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, &uses]).await.unwrap();
        ids.push(id);
    }
    // The survivor's nonzero non-destruction protection keeps it whole.
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,1)", &[&ids[1], &ATTR_NON_DESTRUCTIBLE]).await.unwrap();
    let fences = crate::services::effects::online::capture_online_fences(&c.world, &[owner]).await;
    let _market = crate::services::market::lock().await;
    let (effects, refresh, destroyed) = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        let fences = fences.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let mut destroyed = Vec::new();
            let loss = begin_loss_tx(tx, outbox, &sde, owner, now, |item| destroyed.push(item.item_id)).await?;
            let refresh = finish_loss_tx(tx, outbox, &sde, &fences, owner, now, loss).await?;
            Ok::<_, CallError>((refresh, destroyed))
        })
    }, |(refresh, destroyed), outbox| async move { Ok((outbox.into_entries(), refresh, destroyed)) }).await.unwrap();
    assert_eq!(destroyed, [ids[0]]);
    assert!(!refresh.implants_changed && refresh.boosters_changed);
    let notices: Vec<_> = effects.iter().filter_map(|effect| match effect {
        Effect::Notification(Notification::Pilot { event, args, .. }) => Some((event.as_str(), args.as_slice())),
        _ => None,
    }).collect();
    let names: Vec<_> = notices.iter().map(|(event, _)| *event).collect();
    assert_eq!(names, ["OnItemChange", "OnNonDiminishingInjectionsRemoved", "OnNonDiminishingInjectionsAdded", crate::client::BRAIN_REFRESH, "OnServerBoostersChanged", "OnCloneDestruction"]);
    assert_eq!(notices[2].1, [Value::Int(3)].as_slice());
    let surviving = crate::dao::items::brain::get(&db, owner, ids[1]).await.unwrap().unwrap();
    assert_eq!(surviving.expires_at, Some(expiry));
    assert!(dogma::overrides(&db, ids[1]).await.unwrap().iter().any(|o| o.attribute_id == ATTR_NON_DESTRUCTIBLE && o.value == 1.0));
    crate::scheduler::cancel(&db, &format!("{}:{}", completion::KIND, owner)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn body_loss_respects_protection_order_and_refuses_nonfinite() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let owner = f.character;
    let plain = c.sde.types.iter()
        .find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)
            && !t.attribute(ATTR_NON_DESTRUCTIBLE).is_some_and(|v| v.is_finite() && v != 0.0))
        .expect("unprotected implant type").type_id;
    let typed = c.sde.types.iter()
        .find(|t| t.category_id == Some(CATEGORY_IMPLANT) && t.attribute(ATTR_NON_DESTRUCTIBLE).is_some_and(|v| v.is_finite() && v != 0.0))
        .or_else(|| c.sde.types.iter().find(|t| t.attribute(ATTR_NON_DESTRUCTIBLE).is_some_and(|v| v.is_finite() && v != 0.0)))
        .expect("non-destruction-protected type").type_id;
    let now = filetime_now();
    let expiry = now + FILETIME_MINUTE;
    // Instance override, then type value, then the SDE attribute default, then
    // zero: a plain row dies, an override of zero defeats type protection, and
    // every nonzero finite protection keeps its row and metadata.
    let lost = implant_row(&db, owner, plain, 1, None, None, None).await;
    let kept = implant_row(&db, owner, plain, 2, Some(1.0), Some(expiry), Some(vec![1, 2])).await;
    let mut doomed = vec![lost, implant_row(&db, owner, typed, 3, Some(0.0), None, None).await];
    let survivors = vec![kept, implant_row(&db, owner, typed, 4, None, None, None).await];
    let (clone, dormant) = dormant_clone(&db, owner, c.sde.map.stations().next().unwrap().station_id, plain).await;
    let fences = crate::services::effects::online::capture_online_fences(&c.world, &[owner]).await;
    let _market = crate::services::market::lock().await;
    let (refresh, mut destroyed) = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        let fences = fences.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            let mut destroyed = Vec::new();
            let loss = begin_loss_tx(tx, outbox, &sde, owner, now, |item| destroyed.push(item.item_id)).await?;
            let refresh = finish_loss_tx(tx, outbox, &sde, &fences, owner, now, loss).await?;
            Ok::<_, CallError>((refresh, destroyed))
        })
    }, |value, _outbox| async move { Ok(value) }).await.unwrap();
    assert!(refresh.implants_changed && !refresh.boosters_changed);
    destroyed.sort_unstable();
    doomed.sort_unstable();
    assert_eq!(destroyed, doomed);
    let kept_row = crate::dao::items::brain::get(&db, owner, kept).await.unwrap().unwrap();
    assert_eq!((kept_row.expires_at, kept_row.effect_ids.as_deref()), (Some(expiry), Some([1, 2].as_slice())));
    assert!(dogma::overrides(&db, kept).await.unwrap().iter().any(|o| o.attribute_id == ATTR_NON_DESTRUCTIBLE && o.value == 1.0));
    for item in &survivors {
        assert!(crate::dao::items::brain::get(&db, owner, *item).await.unwrap().is_some());
    }
    assert_eq!(clones::list(&db, owner, clone).await.unwrap().iter().map(|i| i.item_id).collect::<Vec<_>>(), [dormant]);
    // A non-finite protection value refuses the whole loss instead of deleting.
    let bad = implant_row(&db, owner, plain, 5, Some(f64::NAN), None, None).await;
    let refused = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, owner).await.map_err(super::super::db_err)?;
            begin_loss_tx(tx, outbox, &sde, owner, now, |_| {}).await
        })
    }, |value, _outbox| async move { Ok(value) }).await;
    assert!(matches!(refused, Err(CallError::Internal(_))));
    assert!(crate::dao::items::brain::get(&db, owner, bad).await.unwrap().is_some());
    drop_clones(&db, owner).await;
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn projected_capacity_changes_at_completion_without_persisting() {
    use infinity_core::{consts::{dogma::ATTR_MAX_JUMP_CLONES, SKILL_INFOMORPH_PSYCHOLOGY}};
    use crate::services::fit::Key;
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
        (ty.category_id == Some(infinity_core::consts::CATEGORY_SKILL) && primary != secondary).then_some((ty.type_id, primary, secondary))
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
        assert_eq!(training::schedule(&c.sde, &mut row, &attrs, 0).unwrap(), Some(expected));
        assert_eq!(row.end_time, Some(expected));
    }
    fixture::cleanup(&db, f).await.unwrap();
}
