//! Payment ordering against the real journal and daily-goal host.
//! Fixture jumps use the production codec and driver, not a space actor.
use super::*;
use crate::{dao::{accounts::subscriptions, progress::{self as journal, DeliveryState}}, db::outbox::SourceKey, services::daily_goals::Error};
use prost_reflect::ReflectMessage;

async fn rows(f: &PaymentFixture, b: &periods::Bounds) -> Vec<instances::GoalRow> {
    let mut result = instances::period(&f.db, f.owner.character, "day", b.day_start).await.unwrap();
    result.extend(instances::period(&f.db, f.owner.character, "month", b.month_start).await.unwrap());
    result
}
async fn applied(f: &PaymentFixture, key: &crate::progress::DeliveryKey, expected: bool) {
    let state = journal::delivery(&f.db, key).await.unwrap();
    assert!(if expected { matches!(state, DeliveryState::Applied) } else { matches!(state, DeliveryState::Pending(_)) });
}
async fn completion(f: &PaymentFixture, id: &str) -> bool {
    let source = journal::source(&f.db, "business", SourceKey::canonical("DailyGoalCompleted", id, 0).as_str()).await.unwrap();
    if let Some(event) = &source { assert_eq!(event.ordinal, 0); }
    source.is_some()
}

// The holder owns only the selected fixture row. Dropping the sender also releases
// the transaction if the scenario panics; the error forces an actual rollback.
async fn hold(f: &PaymentFixture, id: Option<String>) -> (i32, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let db = f.db.clone();
    let owner = f.owner.character;
    let user = f.owner.account;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let ready = std::sync::Arc::new(tokio::sync::Mutex::new(Some(ready_tx)));
    let release = std::sync::Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let task = tokio::spawn(async move {
        let _: Result<(), crate::db::Error> = db.transaction(|tx, _: &mut ()| {
            let id = id.clone();
            let ready = ready.clone();
            let release = release.clone();
            Box::pin(async move {
                if let Some(id) = id { instances::lock_sorted(tx, owner, &[id]).await?; }
                else { subscriptions::lock(tx, user).await?; }
                let pid: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
                ready.lock().await.take().unwrap().send(pid).unwrap();
                let _ = release.lock().await.take().unwrap().await;
                Err(crate::db::Error::Progress(anyhow::anyhow!("release test row lock")))
            })
        }, |(), ()| async { Ok(()) }).await;
    });
    (ready_rx.await.unwrap(), release_tx, task)
}
async fn blocked(db: &crate::db::Db, blocker: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let row = db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await.expect("pg_stat_activity and pg_blocking_pids access required");
            if row.get::<_, bool>("blocked") { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("payment did not block on fixture row; catalog visibility is required");
}

async fn run(case: u8) {
    let root = std::env::temp_dir().join(format!("daily-payment-races-{}", crate::handshake::random_u64()));
    let mut fixture = None;
    let result = crate::tests::catch_scenario(async {
        let (db, resources) = current_environment(&root).await;
        fixture = Some(PaymentFixture::fresh_profile(db, resources).await);
        let f = fixture.as_mut().unwrap();
        let at = crate::client::filetime_now();
        let bounds = periods::bounds(&f.db, at).await.unwrap();
        let views = f.ensure_current(at).await;
        let jump = views.iter().find(|v| v.goal.kind == "day" && v.goal.category == "ordinary" && v.snapshot.template.key == "jump").unwrap().goal.clone();
        assert_eq!((jump.target, jump.progress), (20, 0));
        f.fund(7000, at).await;
        match case {
            0 => {
                let ids: Vec<_> = views.iter().filter(|v| v.goal.kind == "day" && v.goal.category == "ordinary").take(2).map(|v| v.goal.goal_id.clone()).collect();
                let (a, b) = tokio::join!(payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &ids[0], at), payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &ids[1], at));
                a.unwrap(); b.unwrap();
                let mut receipts = Vec::new();
                for id in &ids { receipts.push(receipts::payment(&f.db, f.owner.character, id).await.unwrap().unwrap()); }
                receipts.sort_by_key(|r| r.ordinal);
                assert_eq!((receipts[0].ordinal, receipts[0].amount, receipts[1].ordinal, receipts[1].amount), (1, 2500, 2, 4500));
                assert_eq!(f.balance().await, 0);
                assert_eq!(receipts::payment_count(&f.db, f.owner.character, bounds.day_start).await.unwrap(), 2);
                payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &receipts[0].goal_id, bounds.month_end + FILETIME_DAY).await.unwrap();
                assert_eq!(f.balance().await, 0);
                assert_eq!(receipts::payment(&f.db, f.owner.character, &receipts[0].goal_id).await.unwrap().unwrap(), receipts[0]);
            }
            1 => {
                let mut notices = crate::gateway::tests::notice_stream(&f.resources.world, [9; 16], f.owner.character, 128);
                for _ in 0..19 { let key = f.publish_jump(at).await; f.drain(&key).await; }
                let key = f.publish_jump(at).await;
                let ((), result) = tokio::join!(f.drain(&key), payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &jump.goal_id, at));
                assert!(matches!(result, Err(Error::Forbidden)));
                applied(f, &key, true).await;
                let goal = instances::get(&f.db, f.owner.character, &jump.goal_id).await.unwrap().unwrap();
                assert_eq!(goal.progress, 20); assert!(goal.completed_at.is_some() && !goal.paid);
                assert!(completion(f, &jump.goal_id).await);
                assert_eq!(f.balance().await, 7000);
                assert!(receipts::payment(&f.db, f.owner.character, &jump.goal_id).await.unwrap().is_none());
                assert!(matches!(payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &jump.goal_id, at).await, Err(Error::Forbidden)));
                let day = instances::period(&f.db, f.owner.character, "day", bounds.day_start).await.unwrap();
                let bonus = day.iter().find(|g| g.category == "bonus").unwrap();
                assert_eq!(bonus.progress, 1);
                let month = instances::period(&f.db, f.owner.character, "month", bounds.month_start).await.unwrap();
                assert!(month.iter().all(|g| g.progress == 0));
                let other = views.iter().find(|v| v.goal.kind == "day" && v.goal.category == "ordinary" && v.goal.goal_id != jump.goal_id).unwrap();
                payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &other.goal.goal_id, at).await.unwrap();
                assert_eq!(f.balance().await, 4500);
                assert_eq!(receipts::payment(&f.db, f.owner.character, &other.goal.goal_id).await.unwrap().unwrap().amount, 2500);
                let uuid = crate::uuid_text::parse(&jump.goal_id, crate::uuid_text::Case::Either).unwrap();
                let mut seen_progress = false;
                let mut seen_completed = false;
                while let Ok(frame) = notices.rx.try_recv() {
                    let (_, message) = crate::gateway::tests::decode_notice(&frame);
                    let name = message.descriptor().full_name().to_owned();
                    if name == "eve_public.dailygoal.api.ProgressedNotice" {
                        let goal = message.get_field_by_name("goal").unwrap();
                        let id = goal.as_message().unwrap().get_field_by_name("uuid").unwrap();
                        if &id.as_bytes().unwrap()[..] == &uuid[..] && message.get_field_by_name("current_progress").unwrap().as_u64().unwrap() == 20 { seen_progress = true; }
                    } else if name == "eve_public.dailygoal.api.CompletedNotice" && seen_progress {
                        let goal = message.get_field_by_name("goal").unwrap();
                        let id = goal.as_message().unwrap().get_field_by_name("uuid").unwrap();
                        if &id.as_bytes().unwrap()[..] == &uuid[..] { seen_completed = true; }
                    }
                }
                assert!(seen_progress && seen_completed);
            }
            2 | 3 => {
                let mut keys = Vec::new();
                if case == 2 { for _ in 0..13 { keys.push(f.publish_jump(at).await); } }
                let before = rows(f, &bounds).await;
                let (pid, release, holder) = hold(f, if case == 2 { Some(jump.goal_id.clone()) } else { None }).await;
                let pay = payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &jump.goal_id, at);
                let controller = async {
                    blocked(&f.db, pid).await;
                    let late = f.publish_jump(at).await;
                    release.send(()).unwrap();
                    holder.await.unwrap();
                    late
                };
                let (result, late) = tokio::join!(pay, controller);
                if case == 2 {
                    keys.push(late);
                    assert!(matches!(result, Err(Error::Forbidden)));
                    for key in &keys { applied(f, key, false).await; }
                    assert_eq!(rows(f, &bounds).await, before);
                    assert_eq!(f.balance().await, 7000);
                    assert!(receipts::payment(&f.db, f.owner.character, &jump.goal_id).await.unwrap().is_none());
                    for goal in &before { assert!(!completion(f, &goal.goal_id).await); }
                } else {
                    result.unwrap();
                    applied(f, &late, false).await;
                    let paid = instances::get(&f.db, f.owner.character, &jump.goal_id).await.unwrap().unwrap();
                    assert!(paid.paid && paid.completed_at.is_some()); assert_eq!(paid.progress, 20);
                    let receipt = receipts::payment(&f.db, f.owner.character, &jump.goal_id).await.unwrap().unwrap();
                    assert_eq!((receipt.ordinal, receipt.amount), (1, 2500));
                    assert_eq!(f.balance().await, 4500);
                    assert!(completion(f, &jump.goal_id).await);
                    let after = rows(f, &bounds).await;
                    f.drain(&late).await;
                    applied(f, &late, true).await;
                    assert_eq!(rows(f, &bounds).await, after);
                    assert_eq!(f.balance().await, 4500);
                    assert_eq!(receipts::payment(&f.db, f.owner.character, &jump.goal_id).await.unwrap().unwrap(), receipt);
                }
            }
            _ => unreachable!(),
        }
    }).await;
    let cleanup = crate::tests::catch_scenario(async { if let Some(f) = &fixture { f.cleanup().await; } }).await;
    let restore = crate::tests::catch_scenario(async {
        if crate::lua::HOST.get().is_some() {
            let version = std::fs::read_to_string(root.join("payment-version")).unwrap();
            authored(&root, &version); crate::lua::HOST.get().unwrap().reload().await.unwrap();
        }
    }).await;
    let removed = std::fs::remove_dir_all(&root);
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
    if let Err(panic) = restore { std::panic::resume_unwind(panic); }
    removed.unwrap();
}

macro_rules! race_test {
    ($name:ident, $marker:literal, $case:expr) => {
        #[tokio::test]
        #[ignore = "requires dedicated initialized SDE database and externally serialized Lua publication"]
        async fn $name() {
            if std::env::var_os($marker).is_none() {
                let name = concat!("lua::tests::daily_goals::payments::races::", stringify!($name));
                let status = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", name, "--ignored", "--nocapture"]).env($marker, "1").status().unwrap();
                assert!(status.success(), "payment race child failed");
                return;
            }
            run($case).await;
        }
    };
}
race_test!(concurrent_distinct_payments_and_replay, "INFINITY_DAILY_RACE_PAY_CHILD", 0);
race_test!(natural_completion_wins_payment, "INFINITY_DAILY_RACE_NATURAL_CHILD", 1);
race_test!(pending_snapshot_growth_reprepares, "INFINITY_DAILY_RACE_GROWTH_CHILD", 2);
race_test!(late_source_after_verified_snapshot, "INFINITY_DAILY_RACE_LATE_CHILD", 3);
