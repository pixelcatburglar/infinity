//! /setattr value policy and zero-rate lifecycle regressions; the shared remap
//! rollback test owns the Gm rollback arm. The unique MAX boundary drives the
//! real implant and booster modifiers through the production setter and the
//! SKILL_HANDLER GetAttributes answer.
use super::*;

#[test]
fn gm_value_rounds_and_bounds_after_rounding() {
    for (value, base) in [(0.0, 0), (0.5, 1), (19.4, 19), (i32::MAX as f64 + 0.4, i32::MAX)] {
        assert_eq!(rounded_gm_base(value).unwrap_or_else(|_| panic!("{value}")), base);
    }
    for value in [-0.1, f64::NAN, f64::INFINITY, i32::MAX as f64 + 0.5, 1e30] {
        assert!(matches!(rounded_gm_base(value), Err(CallError::User { .. })), "{value}");
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn gm_zero_blocks_and_restores_without_entitlement_or_backfill() {
    use crate::{db::Db, services::tests::{client, fixture}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let mut recipient = None;
    let scenario = crate::tests::catch_scenario(async {
    fixture::seed_skill(&db, f.character).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let type_id = use_real_skill(&db, f.character, &c.sde).await;
    let rank = training::skill_type(&c.sde, type_id).rank;
    let (dest1, dest2) = (super::super::super::formulas::sp_for_level(rank, 1), super::super::super::formulas::sp_for_level(rank, 2));
    let sde = c.sde.clone();
    let world = c.world.clone();
    let (notify, mut notices) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(f.character, notify);
    attributes::initialize_respec(&db, f.character, 3).await.unwrap();
    db.execute("UPDATE game.character_respec SET free_respecs=3, last_respec_date=7, next_timed_respec=77 WHERE character_id=$1", &[&f.character]).await.unwrap();
    db.execute("UPDATE game.characters SET free_skill_points=4242 WHERE character_id=$1", &[&f.character]).await.unwrap();
    let now = filetime_now();
    let (start1, end1, start2, end2) = (now - 8 * FILETIME_DAY, now - 4 * FILETIME_DAY, now - 4 * FILETIME_DAY, now + FILETIME_DAY);
    db.execute("UPDATE game.skill_queue SET start_sp=0, dest_sp=$2, start_time=$3, end_time=$4 WHERE character_id=$1", &[&f.character, &dest1, &start1, &end1]).await.unwrap();
    queue::insert(&db, f.character, &queue::QueueEntry { position: 1, type_id, to_level: 2, start_sp: dest1, dest_sp: dest2, start_time: Some(start2), end_time: Some(end2) }).await.unwrap();
    let booster: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_expires_at) VALUES (1,$1,$1,$2,true,$3) RETURNING item_id", &[&f.character, &infinity_core::consts::FLAG_BOOSTER, &(now + FILETIME_DAY)]).await.unwrap().get("item_id");
    let banked = training::points_at(dest1, dest2, start2, end2, now);
    let owner_ship = crate::dao::items::creation::rookie_ship(&db, infinity_core::consts::TYPE_CAPSULE, f.character, sde.map.stations().next().unwrap().station_id, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    crate::dao::characters::selection::set_ship(&db, f.character, owner_ship).await.unwrap();
    recipient = Some({
        let market = crate::services::market::lock().await;
        crate::services::tests::fixture::LiveRecipient::cold(&world, f.character, None, &market).await
    });
    let live = recipient.as_ref().unwrap();
    let unchanged = live.read(|fit| fit.value(Key::Ship, ATTR_CAPACITY)).await;
    let fences = crate::services::effects::online::capture_online_fences(&world, &[f.character]).await;
    {
        let _market = crate::services::market::lock().await;
        db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone(); let fences = fences.clone();
            Box::pin(async move { apply_tx(tx, outbox, &sde, &fences, f.character, &[0; 5], ChangeKind::Gm, now).await })
        }, |value, outbox| {
            let world = &world;
            async move { online::dispatch_changed_world(value, outbox, world) }
        }).await.unwrap_or_else(|_| panic!("setattr 0"));
    }
    assert_eq!(live.read(|fit| CHAR_ATTRIBUTES.map(|id| fit.value(Key::Char, id as i32))).await, [0.0; 5]);
    assert_eq!(live.read(|fit| fit.value(Key::Ship, ATTR_CAPACITY)).await, unchanged);
    let respec = attributes::respec(&db, f.character).await.unwrap().unwrap();
    assert_eq!(respec.bases.as_deref(), Some(&[0; 5][..]));
    assert_eq!((respec.free_respecs, respec.last_respec_date, respec.next_timed_respec), (3, Some(7), Some(77)));
    let skill = skill_dao::current(&db, f.character).await.unwrap();
    assert_eq!((skill[0].level, skill[0].skill_points), (1, banked));
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), 4242);
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap().get::<_, i64>("n"), 1);
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].start_sp, rows[0].start_time, rows[0].end_time), (banked, Some(now), None));
    let job = format!("{}:{}", completion::KIND, f.character);
    assert_eq!(db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().get::<_, i64>("due_at"), now + FILETIME_DAY);
    assert!(crate::dao::items::brain::get(&db, f.character, booster).await.unwrap().is_some());
    let events: Vec<String> = std::iter::from_fn(|| notices.try_recv().ok()).map(|(name, _)| name).collect();
    assert_eq!(events.iter().filter(|name| name.as_str() == "OnServerSkillsChanged").count(), 1);
    assert_eq!(events.iter().filter(|name| name.as_str() == "OnRespecInfoChanged").count(), 1);
    assert_eq!(events.iter().filter(|name| name.as_str() == BRAIN_REFRESH).count(), 1);
    // Re-settling later banks nothing for the open-ended blocked interval.
    let later = filetime_now();
    {
        let _market = crate::services::market::lock().await;
        db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
                completion::advance_tx(tx, outbox, &sde, f.character, later).await.map(|_| ())
            })
        }, |value, _| async move { Ok(value) }).await.unwrap_or_else(|_: CallError| panic!("settle blocked queue"));
    }
    assert_eq!(skill_dao::current(&db, f.character).await.unwrap()[0].skill_points, banked);
    assert_eq!(gm_set_attributes(&db, &world, &sde, f.character, 20.0).await.unwrap_or_else(|_| panic!("setattr 20")), 20);
    assert_eq!(live.read(|fit| CHAR_ATTRIBUTES.map(|id| fit.value(Key::Char, id as i32))).await, [20.0; 5]);
    assert_eq!(live.read(|fit| fit.value(Key::Ship, ATTR_CAPACITY)).await, unchanged);
    let rows = queue::list(&db, f.character).await.unwrap();
    let resume = rows[0].start_time.unwrap();
    assert!(resume >= later && rows[0].end_time.unwrap() > resume);
    assert_eq!(skill_dao::current(&db, f.character).await.unwrap()[0].skill_points, banked);
    // A user-paused queue stays fully untimed through a GM base write.
    db.execute("UPDATE game.skill_queue SET start_time=NULL, end_time=NULL WHERE character_id=$1", &[&f.character]).await.unwrap();
    gm_set_attributes(&db, &world, &sde, f.character, 0.0).await.unwrap_or_else(|_| panic!("setattr on pause"));
    assert_eq!(live.read(|fit| CHAR_ATTRIBUTES.map(|id| fit.value(Key::Char, id as i32))).await, [0.0; 5]);
    assert_eq!(live.read(|fit| fit.value(Key::Ship, ATTR_CAPACITY)).await, unchanged);
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!((rows[0].start_time, rows[0].end_time), (None, None));
    world.unregister_pilot_generation(f.character, None);
    }).await;
    if let Some(live) = recipient {
        live.close().await;
    }
    cleanup_after(&db, f).await;
    fixture::cleanup(&db, f).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}

async fn cleanup_after(db: &crate::db::Db, f: crate::services::tests::fixture::Fixture) {
    crate::scheduler::cancel(db, &format!("{}:{}", completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn gm_max_base_stays_finite_through_real_implant_and_booster() {
    use infinity_core::consts::{CATEGORY_IMPLANT, FLAG_BOOSTER, FLAG_IMPLANT, GROUP_BOOSTER, dogma::{ATTR_BOOSTERNESS, ATTR_BOOSTER_DURATION, ATTR_BOOSTER_MAX_CHAR_AGE_HOURS, ATTR_BOOSTER_LAST_INJECTION_DATETIME, ATTR_IMPLANTNESS}};
    use crate::{db::Db, services::{fit::OP_MOD_ADD, tests::{client, fixture}}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let scenario = crate::tests::catch_scenario(async {
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let world = c.world.clone();
    let sde = c.sde.clone();
    let now = filetime_now();
    let primary_id = sde.dogma.attributes_by_name("primaryAttribute").next().expect("primary skill attribute").attribute_id;
    let secondary_id = sde.dogma.attributes_by_name("secondaryAttribute").next().expect("secondary skill attribute").attribute_id;
    // Real implant and booster ItemModifier effects on charID both apply;
    // their attribute values push the effective attributes past MAX.
    let modifier = |booster: bool| c.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT)
        && (t.group_id == Some(GROUP_BOOSTER)) == booster && t.attribute(if booster { ATTR_BOOSTERNESS } else { ATTR_IMPLANTNESS }).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
        && (!booster || (t.attribute(ATTR_BOOSTER_DURATION).is_some() && t.attribute(ATTR_BOOSTER_MAX_CHAR_AGE_HOURS).is_none() && t.attribute(ATTR_BOOSTER_LAST_INJECTION_DATETIME).is_none())))
        .flat_map(|t| t.effects().iter().filter_map(|e| c.sde.dogma.effect(e.effect_id)).filter(|e| e.effect_category.unwrap_or(0) == 0).flat_map(move |e| e.modifiers().iter().map(move |m| (t, e.effect_id, m))))
        .find(|(_, _, m)| m.func == "ItemModifier" && m.domain == "charID" && m.op == OP_MOD_ADD && CHAR_ATTRIBUTES.contains(&(m.modified as i64)) && m.modified != m.modifying);
    let mut boosted = Vec::new();
    for (flag, found) in [(FLAG_IMPLANT, modifier(false)), (FLAG_BOOSTER, modifier(true))] {
        let (ty, effect_id, m) = found.unwrap_or_else(|| panic!("no real char-training modifier for flag {flag} in SDE"));
        let expiry = if flag == FLAG_BOOSTER { Some(now + FILETIME_DAY) } else { None };
        let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,$5,$6) RETURNING item_id",
            &[&ty.type_id, &f.character, &flag, &Some(now), &expiry, &Some(vec![effect_id])]).await.unwrap().get("item_id");
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &m.modifying]).await.unwrap();
        boosted.push(i64::from(m.modified));
    }
    assert_eq!(gm_set_attributes(&db, &world, &sde, f.character, i32::MAX as f64 + 0.4).await.unwrap_or_else(|_| panic!("setattr MAX")), i32::MAX);
    let effective = super::super::super::attributes::character_attributes_db(&db, &sde, f.character).await.unwrap_or_else(|_| panic!("effective attributes"));
    assert!(effective.values().all(|v| v.is_finite() && *v >= i32::MAX as f64));
    assert!(boosted.iter().all(|id| effective[id] > i32::MAX as f64), "implant and booster modifiers must each lift their attribute past MAX once: {boosted:?}");
    let type_id = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SKILL)
        && [primary_id, secondary_id].into_iter().filter_map(|a| t.attribute(a)).any(|a| boosted.contains(&(a as i64)))).expect("SDE skill trained by a boosted attribute").type_id;
    let mut queued = queue::QueueEntry { position: 0, type_id, to_level: 1, start_sp: 0, dest_sp: super::super::super::formulas::sp_for_level(training::skill_type(&sde, type_id).rank, 1), start_time: None, end_time: None };
    assert!(training::schedule(&sde, &mut queued, &effective, now).unwrap_or_else(|_| panic!("schedule under MAX base")).is_some_and(|end| end > now));
    let args: &[Value] = &[];
    let kwargs = Value::Dict(vec![]);
    let call = crate::services::Call { service: crate::services::skills::SKILL_HANDLER, method: "GetAttributes", args, kwargs: &kwargs, bound: false, bind_params: None };
    let Some(Ok(Value::Dict(attrs))) = crate::services::skills::dispatch(&mut c, &call).await else { panic!("GetAttributes dispatch") };
    assert_eq!(attrs.len(), CHAR_ATTRIBUTES.len());
    for id in CHAR_ATTRIBUTES {
        assert_eq!(attrs.iter().filter(|(k, _)| *k == Value::Int(id)).count(), 1, "GetAttributes must hold {id} once");
        assert_eq!(attrs.iter().find(|(k, _)| *k == Value::Int(id)).map(|(_, v)| v), Some(&Value::Float(effective[&id])), "GetAttributes {id}");
    }
    }).await;
    cleanup_after(&db, f).await;
    fixture::cleanup(&db, f).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}
