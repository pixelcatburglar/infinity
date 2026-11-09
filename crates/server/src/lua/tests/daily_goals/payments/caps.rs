//! Payment catch-up admission boundaries with the real daily-goal fixture.
//! The production daily-goal policy and progress journal supply all facts and state.
//! These tests require initialized SDE data and do not simulate a space actor jump.
use super::*;
use crate::{dao::progress::{self as journal, DeliveryState}, db::outbox::SourceKey, services::daily_goals::Error};
use prost_reflect::ReflectMessage;

const PENDING_CHILD: &str = "INFINITY_DAILY_PAYMENT_PENDING_CAP_CHILD";
const GOAL_CHILD: &str = "INFINITY_DAILY_PAYMENT_GOAL_CAP_CHILD";
const PENDING_NAME: &str = "lua::tests::daily_goals::payments::caps::pending_cap_and_exact_boundary";
const GOAL_NAME: &str = "lua::tests::daily_goals::payments::caps::independent_goal_cap";

async fn rows(f: &PaymentFixture, bounds: &periods::Bounds) -> Vec<crate::dao::daily_goals::instances::GoalRow> {
    let mut rows = instances::period(&f.db, f.owner.character, "day", bounds.day_start).await.unwrap();
    rows.extend(instances::period(&f.db, f.owner.character, "month", bounds.month_start).await.unwrap());
    rows
}

async fn completion(f: &PaymentFixture, id: &str) -> bool {
    let source = journal::source(&f.db, "business", SourceKey::canonical("DailyGoalCompleted", id, 0).as_str()).await.unwrap();
    if let Some(event) = &source { assert_eq!(event.ordinal, 0); }
    source.is_some()
}

async fn assert_states(f: &PaymentFixture, keys: &[crate::progress::DeliveryKey], applied: bool) {
    for key in keys {
        let state = journal::delivery(&f.db, key).await.unwrap();
        assert!(if applied { matches!(state, DeliveryState::Applied) } else { matches!(state, DeliveryState::Pending(_)) });
    }
}

async fn scenario(pending_cap: bool) {
    let root = std::env::temp_dir().join(format!("daily-payment-cap-{}", crate::handshake::random_u64()));
    let mut fixture = None;
    let result = crate::tests::catch_scenario(async {
        let (db, resources) = current_environment(&root).await;
        fixture = Some(PaymentFixture::fresh_profile(db, resources).await);
        let f = fixture.as_mut().unwrap();
        let at = crate::client::filetime_now();
        let bounds = periods::bounds(&f.db, at).await.unwrap();
        let views = f.ensure_current(at).await;
        assert_eq!(views.len(), 13);
        let jumps: Vec<_> = views.iter().filter(|v| v.goal.kind == "day" && v.goal.category == "ordinary" && v.snapshot.template.key == "jump").collect();
        assert_eq!(jumps.len(), 1);
        let target = jumps[0].goal.clone();
        assert_eq!((target.target, target.progress), (20, 0));
        f.fund(10000, at).await;
        assert_eq!(f.balance().await, 10000);
        let before = rows(f, &bounds).await;
        assert_eq!(before.len(), 13);
        assert!(before.iter().all(|g| g.progress == 0 && g.completed_at.is_none()));
        let mut notices = crate::gateway::tests::notice_stream(&f.resources.world, [9; 16], f.owner.character, 128);
        let mut keys = Vec::new();
        let old_at = bounds.day_start - 1;
        let old = periods::bounds(&f.db, old_at).await.unwrap();
        if pending_cap {
            for _ in 0..14 { keys.push(f.publish_jump(at).await); }
        } else {
            assert!(periods::get(&f.db, f.owner.character, "day", old.day_start).await.unwrap().is_none());
            assert!(instances::period(&f.db, f.owner.character, "day", old.day_start).await.unwrap().is_empty());
            keys.push(f.publish_jump(old_at).await);
        }
        assert_states(f, &keys, false).await;
        assert_eq!(receipts::payment_count(&f.db, f.owner.character, bounds.day_start).await.unwrap(), 0);
        assert!(!completion(f, &target.goal_id).await);
        assert!(matches!(payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &target.goal_id, at).await, Err(Error::Forbidden)));
        assert_eq!(f.balance().await, 10000);
        assert_eq!(rows(f, &bounds).await, before);
        assert_eq!(receipts::payment_count(&f.db, f.owner.character, bounds.day_start).await.unwrap(), 0);
        assert!(receipts::payment(&f.db, f.owner.character, &target.goal_id).await.unwrap().is_none());
        assert_states(f, &keys, false).await;
        for goal in &before { assert!(!completion(f, &goal.goal_id).await); }
        if !pending_cap {
            assert!(periods::get(&f.db, f.owner.character, "day", old.day_start).await.unwrap().is_none());
            assert!(instances::period(&f.db, f.owner.character, "day", old.day_start).await.unwrap().is_empty());
        }
        f.drain(&keys[0]).await;
        assert_states(f, &keys[..1], true).await;
        assert_states(f, &keys[1..], false).await;
        if pending_cap {
            assert_eq!(instances::get(&f.db, f.owner.character, &target.goal_id).await.unwrap().unwrap().progress, 1);
            assert_eq!(journal::pending_for_character(&f.db, &keys[0].consumer_key, f.owner.character, 14).await.unwrap().len(), 13);
        } else {
            assert_eq!(rows(f, &bounds).await, before);
            let old_rows = instances::period(&f.db, f.owner.character, "day", old.day_start).await.unwrap();
            assert_eq!(old_rows.len(), 5);
            let old_jump = old_rows.iter().find(|g| serde_json::from_str::<crate::services::daily_goals::GoalSnapshot>(&g.definition).unwrap().template.key == "jump").unwrap();
            assert_eq!(old_jump.progress, 1);
        }
        payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &target.goal_id, at).await.unwrap();
        assert_eq!(f.balance().await, 7500);
        let paid = instances::get(&f.db, f.owner.character, &target.goal_id).await.unwrap().unwrap();
        assert_eq!(paid.progress, paid.target);
        assert!(paid.paid && paid.completed_at.is_some());
        let receipt = receipts::payment(&f.db, f.owner.character, &target.goal_id).await.unwrap().unwrap();
        assert_eq!((receipt.ordinal, receipt.amount), (1, 2500));
        assert_eq!(receipts::payment_count(&f.db, f.owner.character, bounds.day_start).await.unwrap(), 1);
        assert_states(f, &keys, true).await;
        assert!(completion(f, &target.goal_id).await);
        if pending_cap {
            let uuid = crate::uuid_text::parse(&target.goal_id, crate::uuid_text::Case::Either).unwrap();
            let mut progress = Vec::new();
            while let Ok(frame) = notices.rx.try_recv() {
                let (_, message) = crate::gateway::tests::decode_notice(&frame);
                if message.descriptor().full_name() != "eve_public.dailygoal.api.ProgressedNotice" { continue; }
                let goal = message.get_field_by_name("goal").unwrap();
                let id = goal.as_message().unwrap().get_field_by_name("uuid").unwrap();
                if &id.as_bytes().unwrap()[..] == &uuid[..] {
                    progress.push(message.get_field_by_name("current_progress").unwrap().as_u64().unwrap());
                }
            }
            let mut expected: Vec<u64> = (1..=14).collect();
            expected.push(20);
            assert_eq!(progress, expected);
        }
    }).await;
    let cleanup = crate::tests::catch_scenario(async { if let Some(f) = &fixture { f.cleanup().await; } }).await;
    let restore = crate::tests::catch_scenario(async {
        if crate::lua::HOST.get().is_some() {
            let version = std::fs::read_to_string(root.join("payment-version")).unwrap();
            authored(&root, &version);
            crate::lua::HOST.get().unwrap().reload().await.unwrap();
        }
    }).await;
    let removed = std::fs::remove_dir_all(&root);
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
    if let Err(panic) = restore { std::panic::resume_unwind(panic); }
    removed.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated initialized SDE database and externally serialized Lua publication"]
async fn pending_cap_and_exact_boundary() {
    if std::env::var_os(PENDING_CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", PENDING_NAME, "--ignored", "--nocapture"]).env(PENDING_CHILD, "1").status().unwrap();
        assert!(status.success(), "pending cap child failed");
        return;
    }
    scenario(true).await;
}

#[tokio::test]
#[ignore = "requires dedicated initialized SDE database and externally serialized Lua publication"]
async fn independent_goal_cap() {
    if std::env::var_os(GOAL_CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", GOAL_NAME, "--ignored", "--nocapture"]).env(GOAL_CHILD, "1").status().unwrap();
        assert!(status.success(), "goal cap child failed");
        return;
    }
    scenario(false).await;
}
