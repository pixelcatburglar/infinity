//! Real prepared receipt atomicity and replay against the canonical journal.
use super::*;
use crate::{dao::{lua_content, sovereignty::development}, lua::events::{combat::{BinaryAmount, CombatEntity}, resources::{FactContext, FactOrigin, MiningSlice}, GameEvent}, progress::{apply_once_tx, codec, ApplyOutcome, ClaimMode}};
use crate::dao::items::parent_locks::tests::{backend_pid, wait_blocked};

fn event(system: i64, at: i64, origin: FactOrigin, incarnation: [u8; 16], token: u64) -> journal::JournalEvent {
    let key = format!("NpcDestroyed:{:032x}/1/{token}", u128::from_be_bytes(incarnation));
    let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(system), station_id: None, origin };
    let event = GameEvent::NpcDestroyed { death_id: key.clone(), spawn_generation: 1,
        npc: CombatEntity { item_id: 1, type_id: 1, incarnation, character_id: None, corporation_id: None, alliance_id: None, faction_id: None },
        system_id: system, killer_id: None, killer: None, context, occurred_at: at,
        base_bounty: BinaryAmount { coefficient: "0".into(), exponent: 0 }, static_version: None };
    journal::JournalEvent { source_namespace: "business".into(), source_key: key, schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(),
        occurred_at: at, recorded_at: filetime_now(), payload: codec::encode(&event).unwrap(), context: serde_json::to_string(&context).unwrap(),
        origin: origin.as_str().into(), ordinal: 0, subject_character_id: None }
}
async fn pending(db: &crate::db::Db, key: &DeliveryKey) -> EventEnvelope {
    match journal::delivery(db, key).await.unwrap() { DeliveryState::Pending(event) => event, _ => panic!("expected pending delivery") }
}
async fn ready(resources: &ContentProgressResources, key: &DeliveryKey) -> Arc<dyn PreparedDelivery> {
    match prepare(resources.clone(), key.clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, _ => panic!("expected pending delivery") }
}
async fn apply(db: &crate::db::Db, sde: &Arc<Sde>, key: &DeliveryKey, plan: &Arc<dyn PreparedDelivery>, fail: bool, dispatched: &std::sync::atomic::AtomicBool) -> Result<ApplyOutcome, ApplyError> {
    db.transaction(|tx, outbox: &mut Outbox| {
        let (sde, key, plan) = (sde.clone(), key.clone(), plan.clone());
        Box::pin(async move {
            let baseline = if fail {
                let system = match journal::delivery(tx, &key).await? {
                    DeliveryState::Pending(event) => match event.event { GameEvent::NpcDestroyed { system_id, .. } => system_id, _ => unreachable!() },
                    _ => panic!("expected pending delivery"),
                };
                Some((system, development::read(tx, system).await?, development::checkpoint_before(tx, system, i64::MAX).await?))
            } else { None };
            let result = apply_once_tx(tx, outbox, &sde, &registration(), key, Some(plan.as_ref()), ClaimMode::Blocking).await?;
            if fail {
                let (system, state, checkpoint) = baseline.unwrap();
                assert_ne!(development::read(tx, system).await?, state);
                if checkpoint.is_some() { assert_ne!(development::checkpoint_before(tx, system, i64::MAX).await?, checkpoint); }
                assert!(matches!(result, ApplyOutcome::Applied));
                return Err(ApplyError::Call(db_err(anyhow::anyhow!("injected after apply"))));
            }
            Ok(result)
        })
    }, |result, _outbox| async move { dispatched.store(true, std::sync::atomic::Ordering::SeqCst); Ok(result) }).await
}
fn assert_mined(history: &super::super::activation::PolicyHistory, row: &development::DevelopmentRow, at: i64) {
    let definitions = &history.segment_at(at).unwrap().definitions;
    let start = super::super::model::PointState { at, military: 0.0,
        industrial: 300_000.0 * definitions.industrial_points_per_m3 * definitions.industrial[0].modifier };
    let expected = super::super::rules::advance(start, row.evaluated_at, history).unwrap().industrial;
    let baseline = if row.trend_start_at > at { super::super::rules::advance(start, row.trend_start_at, history).unwrap().industrial } else { 0.0 };
    assert!((row.industrial_points - expected).abs() <= 1e-9);
    assert!((row.industrial_trend_base - baseline).abs() <= 1e-9);
}
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn prepared_receipts_replay_and_rollback_with_retained_policies() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let registry = crate::progress::tests::fixture_registry(&db, crate::progress::Registry::new(vec![], vec![registration()]).unwrap()).await;
    let db = db.with_progress_registry(registry);
    let sde = crate::tests::shared_sde(&db).await;
    let ids: Vec<i64> = sde.map.systems().map(|s| s.solar_system_id).collect();
    let mut vacant = development::observations(&db, &ids).await.unwrap().into_iter()
        .filter(|row| row.system.is_none() && row.development.is_none());
    let system = vacant.next().unwrap().system_id;
    let claimed = vacant.next().unwrap().system_id;
    let inactive = vacant.next().unwrap().system_id;
    let mut expected_ids = development::active_systems(&db, 0, i64::MAX).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 0);
    let old = crate::progress::tests::indices_host(&db, &sde, &world).await;
    let mut old_package = None;
    let incarnation = ((u128::from(crate::handshake::random_u64()) << 64) | u128::from(crate::handshake::random_u64())).to_be_bytes();
    let first_log = (crate::handshake::random_u64() & (i64::MAX as u64 / 2)) as i64 + 1;
    let mining_key = format!("OreMined:{first_log}");
    let mut source_keys: Vec<String> = (1..=9).map(|token| format!("NpcDestroyed:{:032x}/1/{token}", u128::from_be_bytes(incarnation))).collect();
    source_keys.push(mining_key.clone());
    let mut new = None;
    let dispatched = std::sync::atomic::AtomicBool::new(false);
    let result = crate::tests::catch_scenario(async {
    old_package = lua_content::load(&db, NAMESPACE, &old.activation.version).await.unwrap();
    let package = old_package.as_ref().unwrap();
    new = Some(crate::progress::tests::indices_host(&db, &sde, &world).await);
    let resources = ContentProgressResources { db: db.clone(), sde: sde.clone(), world: world.clone(), policies: new.as_ref().unwrap().policies.clone() };
    let now = filetime_now();
    let mut bad = event(system, now, FactOrigin::Qa, incarnation, 5); bad.origin = "natural".into();
    let mut mined = event(system, old.activation.published_at, FactOrigin::Natural, incarnation, 9);
    let context: FactContext = serde_json::from_str(&mined.context).unwrap();
    let mining = GameEvent::OreMined { char_id: 9, ore_type: 2, context, slices: (0..300).map(|i| MiningSlice {
        log_id: first_log + i, occurred_at: old.activation.published_at, quantity: 1, volume_m3: 1000.0,
    }).collect() };
    mined.source_key = mining_key.clone(); mined.kind = mining.kind().into(); mined.payload = codec::encode(&mining).unwrap();
    mined.subject_character_id = Some(9);
    let (consumers, kinds) = db.progress_registry().subscription_arrays();
    let inserted = journal::insert_batch(&db, &[
        event(system, old.activation.published_at, FactOrigin::Natural, incarnation, 1),
        event(system, now, FactOrigin::Natural, incarnation, 2),
        event(system, old.activation.published_at, FactOrigin::Natural, incarnation, 3),
        event(system, 1, FactOrigin::Natural, incarnation, 4), bad,
        event(i64::MAX, now, FactOrigin::Qa, incarnation, 6),
        event(system, now, FactOrigin::Gm, incarnation, 7),
        event(system, old.activation.published_at, FactOrigin::Natural, incarnation, 8),
        mined,
        event(system, filetime_now() + infinity_core::consts::FILETIME_DAY, FactOrigin::Natural, incarnation, 9),
    ], consumers, kinds).await.unwrap();
    let key = |token: u64| { let source = format!("NpcDestroyed:{:032x}/1/{token}", u128::from_be_bytes(incarnation));
        DeliveryKey { consumer_key: CONSUMER_KEY.into(), event_id: inserted.iter().find(|row| row.0 == source).unwrap().1.clone() } };
    let mining_delivery = DeliveryKey { consumer_key: CONSUMER_KEY.into(), event_id: inserted.iter().find(|row| row.0 == mining_key).unwrap().1.clone() };
        let mut death = pending(&db, &key(1)).await;
        let context = match &death.event { GameEvent::NpcDestroyed { context, .. } => *context, _ => unreachable!() };
        assert!(matches!(source::map(&death, &sde).unwrap().unwrap().slices[0].increment, crate::dao::progress::sovereignty::Increment::NpcDeath));
        death.subject_character_id = Some(9);
        assert!(source::map(&death, &sde).is_err());
        death.subject_character_id = None;
        if let GameEvent::NpcDestroyed { context, .. } = &mut death.event { context.system_id = Some(i64::MAX); death.context = serde_json::to_string(context).unwrap(); }
        death.payload = serde_json::to_string(&death.event).unwrap();
        assert!(source::map(&death, &sde).is_err());
        let mut gm = pending(&db, &key(7)).await;
        assert_eq!(source::map(&gm, &sde).unwrap(), None);
        gm.source_key.push('0');
        assert!(source::map(&gm, &sde).is_err());
        assert!(source::map(&pending(&db, &key(6)).await, &sde).is_err());
        death.event = GameEvent::OreMined { char_id: 9, ore_type: 2, context, slices: vec![
            MiningSlice { log_id: 1, occurred_at: 90, quantity: 2, volume_m3: 4.0 },
            MiningSlice { log_id: 2, occurred_at: 80, quantity: 1, volume_m3: 3.0 }] };
        death.kind = death.event.kind().into(); death.payload = codec::encode(&death.event).unwrap();
        death.context = serde_json::to_string(&context).unwrap(); death.source_key = "OreMined:1".into();
        death.occurred_at = 80; death.subject_character_id = Some(9);
        let fact = source::map(&death, &sde).unwrap().unwrap();
        assert_eq!((fact.slices[0].order.slice_id, fact.slices[1].order.slice_id), (2, 1));
        assert!(matches!(fact.slices[1].increment, crate::dao::progress::sovereignty::Increment::MinedM3(4.0)));
        death.ordinal = -1;
        assert!(source::map(&death, &sde).is_err());
        db.execute("UPDATE game.lua_content_versions SET source=$3 WHERE namespace=$1 AND version=$2", &[&package.namespace, &package.version, &b"corrupt".as_slice()]).await.unwrap();
        // External writes never reach the loaded catalogue; prepared reads stay usable.
        let _ = ready(&resources, &key(1)).await;
        assert!(matches!(journal::delivery(&db, &key(1)).await.unwrap(), DeliveryState::Pending(_)));
        db.execute("UPDATE game.lua_content_versions SET source=$3 WHERE namespace=$1 AND version=$2", &[&package.namespace, &package.version, &package.source]).await.unwrap();
        let first = ready(&resources, &key(1)).await;
        let second = ready(&resources, &key(2)).await;
        let before = development::read(&db, system).await.unwrap();
        let checkpoints = development::checkpoint_before(&db, system, i64::MAX).await.unwrap();
        assert!(apply(&db, &sde, &key(1), &first, true, &dispatched).await.is_err());
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(development::read(&db, system).await.unwrap(), before);
        assert_eq!(development::checkpoint_before(&db, system, i64::MAX).await.unwrap(), checkpoints);
        assert!(matches!(journal::delivery(&db, &key(1)).await.unwrap(), DeliveryState::Pending(_)));
        assert!(matches!(apply(&db, &sde, &key(1), &first, false, &dispatched).await.unwrap(), ApplyOutcome::Applied));
        assert!(matches!(apply(&db, &sde, &key(2), &second, false, &dispatched).await, Err(ApplyError::Reprepare)));
        assert!(matches!(prepare(resources.clone(), key(1)).await.unwrap(), PrepareOutcome::AlreadyApplied));
        let second = ready(&resources, &key(2)).await;
        assert!(matches!(apply(&db, &sde, &key(2), &second, false, &dispatched).await.unwrap(), ApplyOutcome::Applied));
        let mining_plan = ready(&resources, &mining_delivery).await;
        assert!(matches!(apply(&db, &sde, &mining_delivery, &mining_plan, false, &dispatched).await.unwrap(), ApplyOutcome::Applied));
        let history = activation::load(&resources.policies).unwrap();
        let mined_row = development::read(&db, system).await.unwrap().unwrap();
        assert_mined(&history, &mined_row, old.activation.published_at);
        let definitions = &history.head().definitions;
        let interval = definitions.decay_interval_ticks;
        let at = history.head().starts_at + interval - history.head().starts_at.rem_euclid(interval);
        db.execute(
            "INSERT INTO game.sov_systems (system_id,hub_item_id,claim_corporation_id,alliance_id,claim_time) \
             VALUES ($1,$1,$1,$1,$2)",
            &[&claimed, &at],
        ).await.unwrap();
        assert!(development::read(&db, claimed).await.unwrap().is_none());
        assert!(development::observations(&db, &[system]).await.unwrap()[0].system.is_none());
        expected_ids.extend([system, claimed]);
        expected_ids.sort_unstable();
        let (_, views) = super::super::query::read_views(&db, &sde, &resources.policies, None, filetime_now()).await.unwrap();
        assert_eq!(views.iter().map(|view| view.system_id).collect::<Vec<_>>(), expected_ids);
        assert!(!views.iter().any(|view| view.system_id == inactive));
        assert!(views.windows(2).all(|pair| pair[0].system_id < pair[1].system_id));
        let observation = development::observations(&db, &[claimed]).await.unwrap().pop().unwrap();
        replay::refresh_committed(&db, &world, &resources.policies, &history, observation, at).await.unwrap();
        let boundary = at + definitions.strategic_seconds[0] * infinity_core::consts::FILETIME_SECOND;
        let plan = replay::plan(&db, &sde, &resources.policies, history.clone(), claimed, None, &[], boundary).await.unwrap();
        assert!(!plan.changed());
        let observation = development::observations(&db, &[claimed]).await.unwrap().pop().unwrap();
        assert!(replay::refresh_committed(&db, &world, &resources.policies, &history, observation, boundary).await.unwrap());
        assert_eq!(development::read(&db, claimed).await.unwrap().unwrap().evaluated_at, boundary);
        let definitions = &history.segment_at(old.activation.published_at).unwrap().definitions;
        let checkpoint_state = super::super::rules::advance(super::super::model::PointState {
            at: old.activation.published_at,
            military: definitions.military_points_per_death * definitions.military[0].modifier,
            industrial: 300_000.0 * definitions.industrial_points_per_m3 * definitions.industrial[0].modifier,
        }, old.activation.published_at + 1, &history).unwrap();
        let checkpoint = development::CheckpointRow { system_id: system, checkpoint_at: checkpoint_state.at,
            military_points: checkpoint_state.military, industrial_points: checkpoint_state.industrial, activation_ordinal: old.activation.ordinal };
        development::replace_checkpoints(&db, system, checkpoint.checkpoint_at, &[checkpoint.clone()], history.identities.as_slice()).await.unwrap();
        let late = ready(&resources, &key(3)).await;
        let before_late = development::read(&db, system).await.unwrap().unwrap();
        dispatched.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(apply(&db, &sde, &key(3), &late, true, &dispatched).await.is_err());
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(development::read(&db, system).await.unwrap().unwrap(), before_late);
        assert_eq!(development::checkpoint_before(&db, system, i64::MAX).await.unwrap(), Some(checkpoint));
        assert!(matches!(journal::delivery(&db, &key(3)).await.unwrap(), DeliveryState::Pending(_)));
        assert!(matches!(apply(&db, &sde, &key(3), &late, false, &dispatched).await.unwrap(), ApplyOutcome::Applied));
        let after_late = development::read(&db, system).await.unwrap().unwrap();
        assert_mined(&history, &after_late, old.activation.published_at);
        assert_eq!(after_late.revision, before_late.revision + 1);
        assert!(after_late.military_points > before_late.military_points);
        for token in [4, 7] {
            let excluded = ready(&resources, &key(token)).await;
            assert!(matches!(apply(&db, &sde, &key(token), &excluded, false, &dispatched).await.unwrap(), ApplyOutcome::Applied));
            assert!(matches!(apply(&db, &sde, &key(token), &excluded, false, &dispatched).await.unwrap(), ApplyOutcome::AlreadyApplied));
        }
        assert_eq!(development::read(&db, system).await.unwrap().unwrap(), after_late);
        let replay = ready(&resources, &key(8)).await;
        let observation = development::observations(&db, &[system]).await.unwrap().pop().unwrap();
        let revision = observation.development.as_ref().unwrap().revision;
        let now = filetime_now();
        let refresh_at = now + interval - now.rem_euclid(interval);
        let config = crate::config::tests::test_database().to_pg_config().unwrap();
        let (holder, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let mut holder = deadpool_postgres::ClientWrapper::new(holder, tokio::spawn(async move { connection.await.unwrap(); }));
        let tx = crate::db::Transaction::from(holder.transaction().await.unwrap());
        // Hold the namespace fence, but not the domain row, until the receipt commits.
        lua_content::activations::lock_namespace(&tx, NAMESPACE, true).await.unwrap();
        let blocker = backend_pid(&tx).await;
        let receipt = async {
            wait_blocked(&db, blocker).await;
            let mut outbox = Outbox::default();
            let applied = apply_once_tx(
                &tx, &mut outbox, &sde, &registration(), key(8),
                Some(replay.as_ref()), ClaimMode::Blocking,
            ).await.unwrap();
            assert!(matches!(applied, ApplyOutcome::Applied));
            let receipt_row = development::read(&tx, system).await.unwrap().unwrap();
            tx.commit().await.unwrap();
            crate::db::outbox::dispatch_world((), outbox, &world).unwrap();
            receipt_row
        };
        let maintenance = replay::refresh_committed(
            &db, world.as_ref(), &resources.policies, &history, observation, refresh_at,
        );
        let (receipt_row, refreshed) = tokio::join!(receipt, maintenance);
        assert!(refreshed.unwrap());
        let after_race = development::read(&db, system).await.unwrap().unwrap();
        assert_eq!(after_race.revision, revision + 2);
        let gain = receipt_row.military_points - after_late.military_points;
        assert!(gain > 0.0 && gain <= 100_000.0, "excluded receipts must not be replayed as credit");
        // At the aligned boundary the advanced points are also the new trend baselines.
        let points = super::super::rules::advance(
            super::super::model::PointState { at: receipt_row.evaluated_at,
                military: receipt_row.military_points, industrial: receipt_row.industrial_points },
            refresh_at, &history,
        ).unwrap();
        let expected = development::DevelopmentRow {
            military_points: points.military, industrial_points: points.industrial,
            evaluated_at: refresh_at, revision: revision + 2,
            trend_start_at: refresh_at,
            military_trend_base: points.military, industrial_trend_base: points.industrial,
            ..receipt_row
        };
        assert_eq!(after_race, expected);
        for token in [5, 6] {
            assert!(matches!(prepare(resources.clone(), key(token)).await, Err(ApplyError::Permanent(_))));
            assert!(matches!(journal::delivery(&db, &key(token)).await.unwrap(), DeliveryState::Pending(_)));
        }
        assert!(matches!(prepare(resources.clone(), key(9)).await, Err(ApplyError::Call(_))));
        assert!(matches!(journal::delivery(&db, &key(9)).await.unwrap(), DeliveryState::Pending(_)));
    }).await;
    if let Some(package) = &old_package {
        db.execute("UPDATE game.lua_content_versions SET source=$3 WHERE namespace=$1 AND version=$2", &[&package.namespace, &package.version, &package.source]).await.unwrap();
    }
    for id in [system, claimed, inactive] {
        db.execute("DELETE FROM game.sov_development_checkpoints WHERE system_id=$1", &[&id]).await.unwrap();
        db.execute("DELETE FROM game.sov_development WHERE system_id=$1", &[&id]).await.unwrap();
        db.execute("DELETE FROM game.sov_systems WHERE system_id=$1", &[&id]).await.unwrap();
    }
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE source_namespace='business' AND source_key=ANY($1::text[]))", &[&source_keys]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE source_namespace='business' AND source_key=ANY($1::text[])", &[&source_keys]).await.unwrap();
    for fixture in new.iter().chain(std::iter::once(&old)) {
        db.execute("DELETE FROM game.lua_content_activations WHERE ordinal=$1", &[&fixture.activation.ordinal]).await.unwrap();
        db.execute("DELETE FROM game.lua_content_versions WHERE namespace=$1 AND version=$2", &[&fixture.activation.namespace, &fixture.activation.version]).await.unwrap();
    }
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
