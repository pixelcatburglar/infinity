//! World-side transient NPC wreck coordination: the claim guard's witnessed
//! dispositions posted through a real actor mailbox, and the deferred reserve
//! worker's `RefillWitness` delivery. The fenced receipt proof and the real
//! commit-uncertainty path live in the inventory tests; this file only proves
//! each witness outcome lands on the original actor.

use super::*;
use crate::space::system::npc_wrecks::{NpcWreckAccess, NpcWreckLookup, NpcWreckOperation};
use crate::db::Db;
use std::future::Future;

async fn return_jobs(sender: &tokio::sync::mpsc::UnboundedSender<crate::space::npc::NpcFuture>, jobs: Vec<crate::space::npc::NpcFuture>) -> bool {
    let mut failed = false;
    for job in jobs {
        if let Err(error) = sender.send(job) {
            failed = true;
            error.0.await;
        }
    }
    !failed
}

/// Claim one fresh transient root's deletion through a real actor turn;
/// GM cleanup admits with no pilot or source ball.
async fn claim_delete(world: &Arc<World>, system: i64) -> (i64, NpcWreckGuard) {
    let root = world.with(system, move |s|
        s.npc_wreck_spawn(1, 1, 1, [0.0; 3], 1.0, None, vec![])).await.unwrap().expect("reserved ids");
    let guard = world.with(system, move |s| {
        let access = NpcWreckAccess::GmCleanup { character: 1 };
        let NpcWreckLookup::Ready(view) = s.npc_wreck_recovery_view(&access, root) else { panic!("view") };
        match s.claim_npc_wreck(&view.identity, access,
            NpcWreckOperation::Delete { items: vec![root], persistent_roots: vec![], persistent: vec![] }) {
            NpcWreckLookup::Ready(guard) => guard, _ => panic!("delete claim") }
    }).await.unwrap();
    (root, guard)
}

/// Wait until the registered claim slot reaches `expected`.
async fn status(world: &Arc<World>, claim: &Arc<NpcWreckClaim>, expected: Option<NpcWreckClaimState>) {
    let claim = claim.clone();
    let system = claim.system();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let claim = claim.clone();
            if world.with(system, move |s| s.npc_wreck_claim_status(&claim)).await.flatten() == expected { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("claim status never settled");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn guard_posts_each_witnessed_disposition_and_survives_cancellation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let system = 9_000_000_000_000_i64 + (crate::handshake::random_u64() % 1_000_000) as i64;
    let world = World::new(db.clone(), sde, 1);
    world.system(system).await.unwrap();
    // Fresh guard dropped before any execution: posts rollback. Executing
    // then dropped posts Unknown; an honest finish releases it. A committed
    // dispatch witness still applies once when the owned future is cancelled
    // mid-await; a direct finish applies once through the actor.
    let (_, guard) = claim_delete(&world, system).await;
    let fresh = guard.claim().clone();
    drop(guard);
    status(&world, &fresh, None).await;
    let (root, mut guard) = claim_delete(&world, system).await;
    let executed = guard.claim().clone();
    guard.may_execute();
    drop(guard);
    status(&world, &executed, Some(NpcWreckClaimState::Unknown)).await;
    let settled = world.with(system, move |s| s.finish_npc_wreck(&executed, NpcWreckDisposition::RolledBack)).await.unwrap();
    assert!(settled && world.ball(system, root).await.is_some());
    let (root, guard) = claim_delete(&world, system).await;
    let claim = guard.claim().clone();
    let mut finish = Box::pin(async move {
        let mut guard = guard;
        guard.committed();
        guard.finish(NpcWreckDisposition::Committed).await
    });
    std::future::poll_fn(|cx| {
        assert!(finish.as_mut().poll(cx).is_pending(), "finish must await the actor");
        std::task::Poll::Ready(())
    }).await;
    drop(finish);
    status(&world, &claim, None).await;
    assert!(world.ball(system, root).await.is_none(), "committed delete removes the root");
    // Direct finish also applies once through the actor.
    let (_, guard) = claim_delete(&world, system).await;
    assert!(guard.finish(NpcWreckDisposition::Committed).await);
    // Replace the sender only for the actor turn that consumes IDs. No tick
    // or unrelated actor turn can enqueue into this temporary receiver.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let previous = world.with(system, move |s| {
        let previous = s.deferred_jobs.replace(tx);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while let Some(id) = s.npc_wreck_spawn(1, 1, 1, [0.0; 3], 1.0, None, vec![]) {
                s.remove_ball(id);
            }
        }));
        s.deferred_jobs = previous.clone();
        (previous, outcome)
    }).await.unwrap();
    let sender = previous.0.expect("deferred worker sender");
    let mut jobs = Vec::new();
    while let Ok(job) = rx.try_recv() { jobs.push(job); }
    if let Err(panic) = previous.1 {
        if !return_jobs(&sender, jobs).await { eprintln!("deferred worker stopped; returned jobs completed directly"); }
        std::panic::resume_unwind(panic);
    }
    if jobs.len() != 1 {
        assert!(return_jobs(&sender, jobs).await, "deferred worker stopped; returned jobs completed directly");
        panic!("draining IDs must enqueue exactly one refill");
    }
    let mut worker = jobs.pop().unwrap();
    std::future::poll_fn(|cx| {
        assert!(worker.as_mut().poll(cx).is_pending(), "refill completed before cancellation");
        std::task::Poll::Ready(())
    }).await;
    drop(worker);
    world.with(system, |_| ()).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let retry = crate::config::game().combat.npc_wreck_id_retry_ms;
    let previous = world.with(system, move |s| {
        let previous = s.deferred_jobs.replace(tx);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
            s.npc_wreck_spawn(1, 1, 1, [0.0; 3], 1.0, None, vec![]).is_none()));
        s.deferred_jobs = previous.clone();
        (previous, outcome)
    }).await.unwrap();
    let previous_sender = previous.0.expect("deferred worker sender");
    let mut jobs = Vec::new();
    while let Ok(job) = rx.try_recv() { jobs.push(job); }
    let empty = match previous.1 {
        Ok(empty) => empty,
        Err(panic) => {
            if !return_jobs(&previous_sender, jobs).await { eprintln!("deferred worker stopped; returned jobs completed directly"); }
            std::panic::resume_unwind(panic);
        }
    };
    if !empty || !jobs.is_empty() {
        assert!(return_jobs(&previous_sender, jobs).await, "deferred worker stopped; returned jobs completed directly");
        panic!("cancelled refill must cool down");
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let previous = world.with(system, move |s| {
        s.tick(s.now_ms() + retry + 1);
        let previous = s.deferred_jobs.replace(tx);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
            s.npc_wreck_spawn(1, 1, 1, [0.0; 3], 1.0, None, vec![]).is_none()));
        s.deferred_jobs = previous.clone();
        (previous, outcome)
    }).await.unwrap();
    let previous_sender = previous.0.expect("deferred worker sender");
    let mut jobs = Vec::new();
    while let Ok(job) = rx.try_recv() { jobs.push(job); }
    let empty = match previous.1 {
        Ok(empty) => empty,
        Err(panic) => {
            if !return_jobs(&previous_sender, jobs).await { eprintln!("deferred worker stopped; returned jobs completed directly"); }
            std::panic::resume_unwind(panic);
        }
    };
    if !empty || jobs.len() != 1 {
        assert!(return_jobs(&previous_sender, jobs).await, "deferred worker stopped; returned jobs completed directly");
        panic!("demand after cooldown must enqueue exactly one refill");
    }
    tokio::time::timeout(std::time::Duration::from_secs(30), jobs.pop().unwrap()).await.expect("refill did not finish");
    world.with(system, |_| ()).await.unwrap();
    assert!(world.with(system, move |s| s.npc_wreck_spawn(1, 1, 1, [0.0; 3], 1.0, None, vec![]).is_some()).await.unwrap(),
        "the delivered block must supply a real spawn");
}
