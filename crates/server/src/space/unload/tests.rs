use super::*;
use infinity_core::consts::CATEGORY_SHIP;
use crate::space::tests::system_selection::public_static_system;
use std::{future::{poll_fn, Future}, pin::Pin, sync::atomic::{AtomicUsize, Ordering}, task::Poll};
mod npc_sleep;

#[tokio::test]
#[ignore = "requires performance testing enabled and a dedicated database; run this exact test in its own process"]
async fn world_stop_wins_admission_reloads_once_and_executes_once() {
    let cfg = crate::config::Config::load().unwrap();
    assert!(cfg.gm.performance_testing);
    crate::config::init_game(cfg.game_config()).unwrap();
    let db = Db::connect(&cfg.database).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let id = public_static_system(&sde, 0);
    let world = World::new(db, sde, 0);
    world.system(id).await.unwrap();
    let old = world.running(id).unwrap();
    let identity = old.tx.downgrade();
    let mut ball = old.with(|s| s.balls().next().cloned()).await.unwrap().unwrap();
    let celestial = ball.id;
    let position = ball.pos;
    let gate = Arc::downgrade(&old.gate);
    drop(old);
    ball.id = -1;
    ball.char_id = Some(-1);
    let report = world.unload_idle_systems().await.unwrap();
    assert_eq!(report, UnloadReport { total: 1, unloaded: 1, ..Default::default() });
    assert!(identity.upgrade().is_none());
    let executions = Arc::new(AtomicUsize::new(0));
    let count = executions.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    world.send_to_loaded(id, actor::Job::new(move |s| { count.fetch_add(1, Ordering::SeqCst); s.add_ship(ball); let _ = tx.send(()); })).await.unwrap();
    rx.await.unwrap();
    let new = world.running(id).unwrap();
    assert!(identity.upgrade().is_none());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(new.with(|s| s.ball(-1).is_some()).await.unwrap());
    assert_eq!(new.with(move |s| s.ball(celestial).map(|b| b.pos)).await.unwrap(), Some(position));
    assert!(matches!(world.unload_one(id, &gate, None).await.unwrap(), Outcome::Changed));
    assert!(world.running(id).unwrap().tx.same_channel(&new.tx));
    drop(new);
    let repeat = world.unload_idle_systems().await.unwrap();
    assert_eq!(repeat, UnloadReport { total: 1, busy: 1, ..Default::default() });
}

/// Bounded scheduling barrier for in-flight unload work; not a timer.
async fn until(mut condition: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if condition() { return; }
        tokio::task::yield_now().await;
    }
}

/// Poll one pinned future once, requiring it to queue rather than complete.
async fn poll_pending<F: Future>(mut f: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(f.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    }).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn idle_timeout_unloads_and_reloads_static_scene() {
    crate::tests::init_config();
    assert!(!crate::config::game().gm.performance_testing);
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let id = public_static_system(&sde, 0);
    let world = World::new(db, sde, 0);
    world.system(id).await.unwrap();
    let old = world.running(id).unwrap();
    let (identity, ball) = (old.tx.downgrade(), old.with(|s| s.balls().next().cloned()).await.unwrap().unwrap());
    let (celestial, position) = (ball.id, ball.pos);
    drop(old);
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(1800)).await;
    until(|| identity.upgrade().is_none()).await;
    assert!(identity.upgrade().is_none() && world.running(id).is_none());
    tokio::time::resume();
    world.system(id).await.unwrap();
    assert_eq!(world.with(id, move |s| s.ball(celestial).map(|b| b.pos)).await.unwrap(), Some(position));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn busy_attempt_retries_at_fixed_cold_interval() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let id = public_static_system(&sde, 0);
    let world = World::new(db, sde, 0);
    world.system(id).await.unwrap();
    let extra = world.running(id).unwrap();              // registry + this handle
    extra.with(|_| ()).await.unwrap();                   // actor start barrier: Idle::new ran
    let baseline = extra.tx.strong_count();
    let identity = extra.tx.downgrade();
    let guard = extra.gate.clone().lock_owned().await;   // blocks the deferred flush
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(1800)).await;
    // Senders above the baseline prove a real automatic attempt in flight.
    until(|| extra.tx.strong_count() > baseline).await;
    assert!(extra.tx.strong_count() > baseline);
    drop(guard);
    until(|| extra.tx.strong_count() == baseline).await;
    assert_eq!(extra.tx.strong_count(), baseline);       // coordinator returned
    drop(extra);
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    for _ in 0..50 { tokio::task::yield_now().await; }
    assert!(identity.upgrade().is_some());               // fixed cold interval, not a reopened window
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    until(|| identity.upgrade().is_none()).await;
    assert!(identity.upgrade().is_none() && world.running(id).is_none());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn market_fifo_orders_unload_and_arrival() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (id_a, id_b) = (public_static_system(&sde, 0), public_static_system(&sde, 1));
    let world = World::new(db, sde.clone(), 0);
    world.system(id_a).await.unwrap();
    world.system(id_b).await.unwrap();
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let template = world.with(id_a, |s| s.balls().next().cloned()).await.unwrap().unwrap();
    let (mut ship_a, mut ship_b) = (template.clone(), template);
    for (ship, ship_id) in [(&mut ship_a, -2_i64), (&mut ship_b, -3_i64)] {
        ship.id = ship_id; ship.type_id = kind; ship.char_id = Some(-1);
    }
    // Unload queued first on the market: the stop wins, the arrival reloads once.
    let old = world.running(id_a).unwrap();
    let (identity, gate) = (old.tx.downgrade(), Arc::downgrade(&old.gate));
    drop(old);
    let market = crate::services::market::lock().await;
    let mut u = std::pin::pin!(world.unload_one(id_a, &gate, None));
    poll_pending(u.as_mut()).await;
    let mut a = std::pin::pin!(world.add_ship(id_a, ship_a));
    poll_pending(a.as_mut()).await;
    drop(market);
    let (stopped, arrived) = tokio::join!(u, a);
    assert!(matches!(stopped, Ok(Outcome::Unloaded)));
    arrived.unwrap();
    assert!(identity.upgrade().is_none());
    assert_eq!(world.with(id_a, |s| s.persistent_generation(-2)).await.unwrap(), Some(1));
    // Arrival queued first: it lands on the live actor, the unload is busy.
    let old = world.running(id_b).unwrap();
    let gate = Arc::downgrade(&old.gate);
    drop(old);
    let market = crate::services::market::lock().await;
    let mut a = std::pin::pin!(world.add_ship(id_b, ship_b));
    poll_pending(a.as_mut()).await;
    let mut u = std::pin::pin!(world.unload_one(id_b, &gate, None));
    poll_pending(u.as_mut()).await;
    drop(market);
    let (arrived, stopped) = tokio::join!(a, u);
    arrived.unwrap();
    assert!(matches!(stopped, Ok(Outcome::Busy(_))));
    assert_eq!(world.with(id_b, |s| s.persistent_generation(-3)).await.unwrap(), Some(1));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn npc_bounty_pending_blocks_final_stop_until_thawed() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let id = public_static_system(&sde, 0);
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let account = crate::dao::accounts::authentication::register(&db, &format!("bounty-unload-{seq}"), b"f", 0, 0).await.unwrap().user_id;
    let owner: i64 = db.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,1,1,1,1) RETURNING character_id", &[&account, &format!("bounty-unload-{seq}")]).await.unwrap().get("character_id");
    let world = World::new(db.clone(), sde.clone(), 0);
    world.system(id).await.unwrap();
    // A frozen batch keeps new kills pending; neither dirty nor pending state
    // may pass the final idle-stop check.
    world.with(id, move |s| s.add_npc_bounty(owner, 1, crate::isk::IskAmount::from_cents(10).unwrap(), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), 1_000_000)).await.unwrap().unwrap();
    {
        let shared = world.bounties.character(owner);
        let mut meta = crate::poison::lock_or_exit(shared.meta.lock());
        let batch = meta.batch.unwrap();
        meta.frozen = Some(std::sync::Arc::new(crate::services::bounty::npc::Payout::new(batch, vec![id])));
    }
    world.with(id, move |s| s.add_npc_bounty(owner, 1, crate::isk::IskAmount::from_cents(5).unwrap(), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), 1_000_001)).await.unwrap().unwrap();
    assert!(world.with(id, |s| s.bounty_checkpoint_pending()).await.unwrap());
    let old = world.running(id).unwrap();
    let gate = Arc::downgrade(&old.gate);
    drop(old);
    assert!(matches!(world.unload_one(id, &gate, None).await.unwrap(), Outcome::Busy("npc_bounty_pending")));
    // After the resolution thaws, the pending kill merges, flushes and stops.
    {
        let shared = world.bounties.character(owner);
        let mut meta = crate::poison::lock_or_exit(shared.meta.lock());
        meta.frozen = None;
        meta.pending = Default::default();
    }
    world.with(id, move |s| s.align_npc_bounty(owner)).await;
    world.flush_positions().await.unwrap();
    assert!(!world.with(id, |s| s.bounty_checkpoint_pending()).await.unwrap());
    assert!(matches!(world.unload_one(id, &gate, None).await.unwrap(), Outcome::Unloaded));
    db.execute("DELETE FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id LIKE 'npc-bounty:' || $1::text || ':%'", &[&owner.to_string()]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
