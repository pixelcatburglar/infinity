use super::*;
use tokio::sync::mpsc;

fn fixture() -> (Arc<World>, System) {
    crate::tests::init_config();
    let db = crate::db::Db::unconnected_publication_fixture(([127, 0, 0, 1], 1).into());
    let world = World::new(db, Arc::new(crate::sde::Sde::default()), 0);
    let mut system = System::new(1, Arc::default(), 0);
    system.world = Arc::downgrade(&world);
    (world, system)
}

fn cents(v: i128) -> IskAmount { IskAmount::from_cents(v).unwrap() }

#[test]
fn npc_bounty_actor_cents_revision_and_character_deadline() {
    let (world, mut a) = fixture();
    let mut b = System::new(2, Arc::default(), 0);
    b.world = Arc::downgrade(&world);
    let t0 = 1_000_000_i64;
    a.add_npc_bounty(7, 42, cents(1), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), t0).unwrap();
    b.add_npc_bounty(7, 43, cents(1), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), t0 + 1).unwrap();
    // Two live sources share one character batch and its fixed deadline.
    let batch_a = a.bounties.buckets[&7].batch;
    assert_eq!(batch_a, b.bounties.buckets[&7].batch);
    assert_eq!(batch_a.due_at, t0 + 1200 * FILETIME_SECOND);
    // Integer cents accumulate per kill; every value precomputes before write.
    a.add_npc_bounty(7, 42, cents(2), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), t0 + 2).unwrap();
    let bucket = &a.bounties.buckets[&7];
    assert_eq!((bucket.amount, bucket.kill_count), (cents(3), 2));
    assert!(bucket.revision > bucket.saved_revision);
    // A newer kill outlives an older revision's acknowledgement.
    let checkpoint = a.capture_npc_bounties(None).unwrap().unwrap();
    let captured = a.bounties.buckets[&7].revision;
    a.add_npc_bounty(7, 42, cents(5), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), t0 + 3).unwrap();
    a.apply_bounty_receipt(7, Arc::as_ptr(&checkpoint) as usize, batch_a, captured, a.bounties.incarnation.clone(), CheckpointOutcome::Durable);
    let bucket = &a.bounties.buckets[&7];
    // The captured revision is no longer current: saved stays at 1 and the
    // still-dirty bucket may capture again immediately.
    assert_eq!((bucket.saved_revision, bucket.amount, bucket.kill_count), (1, cents(8), 3));
    assert!(a.bounty_checkpoint_pending());
    let next = a.capture_npc_bounties(None).unwrap().unwrap();
    assert_eq!(next.captures.len(), 1);
    assert_eq!(next.captures[0].pool.amount, cents(8));
    // Pending kills keep their own amounts for the deferred notice; the
    // earliest kill time stays in the shared meta.
    {
        let shared = world.bounties.character(7);
        let mut meta = crate::poison::lock_or_exit(shared.meta.lock());
        meta.frozen = Some(Arc::new(crate::services::bounty::npc::Payout::new(batch_a, vec![1])));
    }
    let low = crate::npc_security::FrozenMultiplier::new(0.5).unwrap();
    let high = crate::npc_security::FrozenMultiplier::new(2.0).unwrap();
    let rate = crate::config::game().crimewatch.security_gain_npc_kill;
    a.add_npc_bounty(7, 42, cents(13), low, t0 + 4).unwrap();
    a.bounties.buckets.get_mut(&7).unwrap().kill_count = i64::MAX - 1;
    a.add_npc_bounty(7, 42, cents(1), high, t0 + 5).unwrap();
    let before = a.bounties.buckets[&7].security_gain;
    {
        let shared = world.bounties.character(7);
        let mut meta = crate::poison::lock_or_exit(shared.meta.lock());
        meta.frozen = None;
        meta.pending.earliest = None;
    }
    a.align_npc_bounty(7);
    let bucket = &a.bounties.buckets[&7];
    let accepted = before.merge(crate::npc_security::SecurityGain::for_kill(rate, low));
    assert_eq!((bucket.amount, bucket.kill_count, bucket.security_gain), (cents(21), i64::MAX, accepted));
    assert_eq!(bucket.pending[0].security_gain, crate::npc_security::SecurityGain::for_kill(rate, high));
    let mut successor = batch_a;
    successor.started_at += 1;
    crate::poison::lock_or_exit(world.bounties.character(7).meta.lock()).batch = Some(successor);
    a.align_npc_bounty(7);
    let reset = &a.bounties.buckets[&7];
    assert_eq!((reset.kill_count, reset.legacy_kill_count, reset.security_gain), (1, 0, crate::npc_security::SecurityGain::for_kill(rate, high)));
    assert!(reset.pending.is_empty());
}

#[tokio::test]
async fn npc_bounty_kill_uses_no_database_or_market_lock() {
    let (world, mut s) = fixture();
    let (tx, mut rx) = mpsc::unbounded_channel();
    s.registry.register_pilot(7, tx);
    let market = crate::services::market::lock().await;
    // Holding the economy lock must not block the actor's pool record.
    s.add_npc_bounty(7, 42, cents(10), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), 1_000).unwrap();
    let (name, args) = rx.try_recv().expect("pool notice missing");
    assert_eq!(name, "OnBountyAddedToPayout");
    assert_eq!(args.len(), 1);
    let dict = &args[0];
    assert_eq!(dict.get("amount"), Some(&Value::Float(0.10)));
    assert_eq!(dict.get("enemyTypeID"), Some(&Value::Int(42)));
    assert_eq!(dict.get("isModified"), Some(&Value::Bool(false)));
    drop(market);
    world.db().assert_publication_fixture_unused();
}
