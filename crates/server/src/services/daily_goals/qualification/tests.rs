use super::*;
use infinity_core::{consts::{FILETIME_DAY, FILETIME_SECOND, GM_ROLES}};
use crate::{client::filetime_now, dao::{accounts::subscriptions as grades, daily_goals::{instances, periods}, lua_content::{self, activations}}, lua::tests::daily_goals::{environment, identity, reload, source}, services::{daily_goals::queries, subscriptions::{deadlines, management, rules}}};

#[tokio::test]
#[ignore = "requires isolated process, dedicated initialized SDE database and externally serialized Lua publication"]
async fn committed_grade_changes_refresh_current_once() {
    const CHILD: &str = "INFINITY_DAILY_QUALIFICATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services::daily_goals::qualification::tests::committed_grade_changes_refresh_current_once", "--ignored", "--nocapture"]).env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    let nonce = crate::handshake::random_u64();
    let root = std::env::temp_dir().join(format!("daily-qualification-{nonce}"));
    let a = format!("qualification-{nonce}-a");
    let b = format!("qualification-{nonce}-b");
    source(&root, &a, false);
    let registry = crate::progress::Registry::new(vec![], vec![crate::services::daily_goals::progress::registration()]).unwrap();
    let (db, resources) = environment(&root, false, registry).await;
    let row = lua_content::load(&db, "daily_goals", &a).await.unwrap().unwrap();
    source(&root, &b, false);
    reload().await;
    let bounds = periods::bounds(&db, filetime_now()).await.unwrap();
    let previous = bounds.month_start - FILETIME_DAY;
    db.transaction(|tx, _: &mut ()| { let row = row.clone(); Box::pin(async move {
        assert!(activations::append_if_changed(tx, &row, previous).await?);
        Ok::<_, crate::db::Error>(())
    }) }, |(), ()| async { Ok(()) }).await.unwrap();
    source(&root, &a, false);
    reload().await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let world = &resources.world;
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    management::set(&db, world, fixture.account, GM_ROLES, fixture.account, rules::ALPHA, None, 0).await.unwrap();
    world.register_pilot(fixture.character, sender);
    world.set_online_session(fixture.character, Some(infinity_core::session::OnlineSession { character_id: fixture.character, user_id: Some(fixture.account), session_id: 1, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    let mut notices = crate::gateway::tests::notice_stream(world, [9; 16], fixture.character, 8);
    let outcome = crate::tests::catch_scenario(async {
        changed(world, fixture.account, filetime_now()).await.unwrap();
        assert!(notices.rx.try_recv().is_ok());
        assert!(notices.rx.try_recv().is_err());
        let id = identity(fixture.character);
        let before = queries::current(&resources, &id, fixture.account, filetime_now()).await.unwrap();
        assert_eq!(before.len(), 13);
        assert!(before.iter().all(|view| !view.current_omega));
        management::set(&db, world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, Some(600), 120).await.unwrap();
        assert!(notices.rx.try_recv().is_ok());
        assert!(notices.rx.try_recv().is_err());
        let upgraded = queries::current(&resources, &id, fixture.account, filetime_now()).await.unwrap();
        assert!(upgraded.iter().all(|view| view.current_omega));
        assert_eq!(before.iter().map(|v| (&v.goal.definition, &v.rewards)).collect::<Vec<_>>(), upgraded.iter().map(|v| (&v.goal.definition, &v.rewards)).collect::<Vec<_>>());
        management::set(&db, world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, Some(1200), 120).await.unwrap();
        assert!(notices.rx.try_recv().is_err());
        let earned = upgraded.iter().find(|v| v.snapshot.template.omega_required).unwrap();
        let update = instances::ProgressUpdate { progress: earned.goal.target, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: Some(filetime_now()), earned_omega: true, paid: false, has_unclaimed: true };
        assert!(instances::update_progress(&db, fixture.character, &earned.goal.goal_id, earned.goal.revision, &update).await.unwrap());
        let grade = grades::by_user(&db, fixture.account).await.unwrap();
        let now = filetime_now();
        assert!(grades::set_grade(&db, fixture.account, rules::OMEGA, Some(now - 2 * FILETIME_SECOND), Some(now - FILETIME_SECOND), grade.revision).await.unwrap());
        let expired = grades::by_user(&db, fixture.account).await.unwrap();
        deadlines::schedule(&db, &expired).await.unwrap();
        let job = crate::dao::jobs::lock_job(&db, &format!("{}:{}", deadlines::KIND, fixture.account)).await.unwrap().unwrap();
        deadlines::handle(db.clone(), world.clone(), job).await.unwrap();
        assert!(notices.rx.try_recv().is_ok());
        assert!(notices.rx.try_recv().is_err());
        let after = queries::current(&resources, &id, fixture.account, filetime_now()).await.unwrap();
        assert!(after.iter().all(|view| !view.current_omega));
        let retained = after.iter().find(|v| v.goal.goal_id == earned.goal.goal_id).unwrap();
        assert!(retained.goal.earned_omega && retained.goal.has_unclaimed);
        management::set(&db, world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, Some(600), 0).await.unwrap();
        assert!(notices.rx.try_recv().is_ok());
        management::set(&db, world, fixture.account, GM_ROLES, fixture.account, rules::ALPHA, None, 0).await.unwrap();
        assert!(notices.rx.try_recv().is_ok());
        assert!(notices.rx.try_recv().is_err());
        management::set(&db, world, fixture.account, GM_ROLES, fixture.account, rules::OMEGA, Some(600), 0).await.unwrap();
        assert!(notices.rx.try_recv().is_ok());
        let grade = grades::by_user(&db, fixture.account).await.unwrap();
        let now = filetime_now();
        assert!(grades::set_grade(&db, fixture.account, rules::OMEGA, Some(now - 2 * FILETIME_SECOND), Some(now - FILETIME_SECOND), grade.revision).await.unwrap());
        management::set(&db, world, fixture.account, GM_ROLES, fixture.account, rules::ALPHA, None, 0).await.unwrap();
        assert!(notices.rx.try_recv().is_ok());
        assert!(notices.rx.try_recv().is_err());
    }).await;
    world.unregister_pilot_generation(fixture.character, None);
    for sql in ["DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", "DELETE FROM game.daily_goal_instances WHERE character_id=$1", "DELETE FROM game.daily_goal_periods WHERE character_id=$1"] { db.execute(sql, &[&fixture.character]).await.unwrap(); }
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
    if let Err(panic) = outcome { std::panic::resume_unwind(panic); }
}
