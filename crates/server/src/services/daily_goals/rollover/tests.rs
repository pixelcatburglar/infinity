use super::*;

#[test]
fn cursor_resets_after_downtime_and_rejects_invalid_payloads() {
    assert_eq!(cursor("{}", 20).unwrap().after_character, None);
    assert_eq!(cursor(r#"{"day_start":10,"after_character":7}"#, 20).unwrap().after_character, None);
    assert_eq!(cursor(r#"{"day_start":20,"after_character":7}"#, 20).unwrap().after_character, Some(7));
    assert!(cursor(r#"{"day_start":20,"after_character":0}"#, 20).is_err());
    assert!(cursor(r#"{"day_start":20,"after_character":null,"extra":1}"#, 20).is_err());
}

#[tokio::test]
#[ignore = "requires dedicated initialized game database"]
async fn rescheduled_job_survives_old_periodic_completion() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = filetime_now();
    let id = format!("daily-rollover-lease-test-{}", crate::handshake::random_u64());
    let result: Result<(), crate::db::Error> = db.transaction(|tx, _: &mut ()| {
        let id = id.clone();
        Box::pin(async move {
            let initial = Job { id: id.clone(), kind: KIND.into(), due_at: now, payload: "{}".into() };
            scheduler::schedule(tx, &initial).await?;
            let claimed = jobs::claim(tx, now, now + infinity_core::consts::FILETIME_SECOND, 1000, &[]).await?;
            let claim = claimed.iter().find(|entry| entry.job.id == id).expect("fixture job was claimed");
            let continuation = Job { payload: r#"{"day_start":1,"after_character":2}"#.into(), ..initial };
            scheduler::schedule(tx, &continuation).await?;
            assert_eq!(jobs::complete_periodic(tx, &id, claim.token, now + infinity_core::consts::FILETIME_SECOND, now).await?, 0);
            assert_eq!(jobs::lock_job(tx, &id).await?.unwrap().payload, continuation.payload);
            Err(crate::db::Error::Progress(anyhow::anyhow!("rollback isolated lease fixture")))
        })
    }, |(), ()| async { Ok(()) }).await;
    assert!(result.is_err());
}


#[tokio::test]
#[ignore = "requires isolated process, dedicated initialized SDE database and externally serialized Lua publication"]
async fn real_rollover_continues_across_online_batches() {
    const CHILD: &str = "INFINITY_DAILY_ROLLOVER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services::daily_goals::rollover::tests::real_rollover_continues_across_online_batches", "--ignored", "--nocapture"]).env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    use infinity_core::consts::FILETIME_DAY;
    use crate::{dao::{daily_goals::{instances, periods}, lua_content::{self, activations}}, lua::tests::daily_goals::{environment, reload, source}};
    let nonce = crate::handshake::random_u64();
    let root = std::env::temp_dir().join(format!("daily-rollover-{nonce}"));
    let a = format!("rollover-{nonce}-a");
    let b = format!("rollover-{nonce}-b");
    source(&root, &a, false);
    let registry = crate::progress::Registry::new(vec![], vec![crate::services::daily_goals::progress::registration()]).unwrap();
    let (db, resources) = environment(&root, false, registry).await;
    let row = lua_content::load(&db, "daily_goals", &a).await.unwrap().unwrap();
    source(&root, &b, false);
    reload().await;
    let now = filetime_now();
    let bounds = periods::bounds(&db, now).await.unwrap();
    let previous = bounds.month_start - 2 * FILETIME_DAY;
    db.transaction(|tx, _: &mut ()| { let row = row.clone(); Box::pin(async move {
        assert!(activations::append_if_changed(tx, &row, previous).await?);
        Ok::<_, crate::db::Error>(())
    }) }, |(), ()| async { Ok(()) }).await.unwrap();
    source(&root, &a, false);
    reload().await;
    let mut profiles = Vec::new();
    for _ in 0..4 { profiles.push(crate::services::tests::fixture::create(&db).await.unwrap()); }
    profiles.sort_by_key(|p| p.character);
    let world = &resources.world;
    let previous_day = bounds.day_start - 1;
    let old = assignment::ensure_at(&resources, profiles[0].account, profiles[0].character, previous_day, false).await.unwrap();
    let prior = instances::period(&db, profiles[0].character, "day", old.bounds.day_start).await.unwrap();
    let goal = &prior[0];
    let update = instances::ProgressUpdate { progress: goal.target, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: Some(previous_day), earned_omega: false, paid: false, has_unclaimed: true };
    assert!(instances::update_progress(&db, profiles[0].character, &goal.goal_id, goal.revision, &update).await.unwrap());
    let mut receivers = Vec::new();
    let mut notices = Vec::new();
    for (index, p) in profiles.iter().take(3).enumerate() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        receivers.push(receiver);
        world.register_pilot(p.character, sender);
        world.set_online_session(p.character, Some(infinity_core::session::OnlineSession { character_id: p.character, user_id: Some(p.account), session_id: 1, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
        notices.push(crate::gateway::tests::notice_stream(world, [index as u8 + 1; 16], p.character, 8));
    }
    let previous_job = jobs::lock_job(&db, KIND).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let stale = Cursor { day_start: bounds.day_start - FILETIME_DAY, after_character: Some(profiles[2].character) };
        let first = Job { id: KIND.into(), kind: KIND.into(), due_at: now, payload: serde_json::to_string(&stale).unwrap() };
        run_batch(db.clone(), world.clone(), first, 2).await.unwrap();
        for p in profiles.iter().take(2) { assert!(periods::get(&db, p.character, "day", bounds.day_start).await.unwrap().is_some()); }
        assert!(periods::get(&db, profiles[2].character, "day", bounds.day_start).await.unwrap().is_none());
        let continuation = jobs::lock_job(&db, KIND).await.unwrap().unwrap();
        let cursor: Cursor = serde_json::from_str(&continuation.payload).unwrap();
        assert_eq!(cursor.after_character, Some(profiles[1].character));
        assert!(continuation.due_at <= filetime_now());
        run_batch(db.clone(), world.clone(), continuation, 2).await.unwrap();
        assert!(periods::get(&db, profiles[2].character, "day", bounds.day_start).await.unwrap().is_some());
        assert!(periods::get(&db, profiles[3].character, "day", bounds.day_start).await.unwrap().is_none());
        let next = jobs::lock_job(&db, KIND).await.unwrap().unwrap();
        assert_eq!(next.due_at, bounds.day_end);
        for stream in &mut notices { assert!(stream.rx.try_recv().is_ok()); assert!(stream.rx.try_recv().is_err()); }
        run_batch(db.clone(), world.clone(), Job { payload: "{}".into(), ..next }, 2).await.unwrap();
        for stream in &mut notices { assert!(stream.rx.try_recv().is_err()); }
        assert!(periods::get(&db, profiles[3].character, "day", old.bounds.day_start).await.unwrap().is_none());
        assert!(instances::get(&db, profiles[0].character, &goal.goal_id).await.unwrap().unwrap().has_unclaimed);
    }).await;
    scheduler::cancel(&db, KIND).await.unwrap();
    if let Some(job) = previous_job { scheduler::schedule(&db, &job).await.unwrap(); }
    for p in profiles {
        world.unregister_pilot_generation(p.character, None);
        for sql in ["DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", "DELETE FROM game.daily_goal_instances WHERE character_id=$1", "DELETE FROM game.daily_goal_periods WHERE character_id=$1"] { db.execute(sql, &[&p.character]).await.unwrap(); }
        crate::services::tests::fixture::cleanup(&db, p).await.unwrap();
    }
    drop(receivers);
    std::fs::remove_dir_all(root).unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
