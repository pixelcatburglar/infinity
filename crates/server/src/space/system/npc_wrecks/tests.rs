//! Actor-owned transient NPC wreck state: claim lifecycle, waiting-IDs
//! parking/resume, exact commit-proof envelopes and unload discardability.
//! Unit tests run on a bare `System`; the one SDE-dependent case loads the
//! shared static dataset from the dedicated test database.

use super::*;
use crate::lua::events::resources::{FactContext, FactOrigin};
use crate::space::system::tests::ball;

const SYSTEM_ID: i64 = 777;
const CHAR: i64 = 5_000_001;
const SHIP: i64 = 9_000_001;
const OWNER: i64 = 5_000_003;
const WRECK_TYPE: i32 = 1;
const LOOT_TYPE: i32 = 8;

fn system() -> System {
    crate::tests::init_config();
    let mut s = System::new(SYSTEM_ID, Arc::new(crate::space::Pilots::default()), 1_000);
    s.insert(ball(SHIP, Some(CHAR), 0.0));
    s
}
fn spawn(s: &mut System, ids: &[i64], loot: &[(i32, i64)]) -> i64 {
    s.npc_wreck_install_ids(ids.to_vec());
    s.npc_wreck_spawn(WRECK_TYPE, OWNER, 1, [0.0; 3], 1.0, None, loot.to_vec()).expect("reserved ids")
}
fn expected(s: &System, root: i64) -> NpcWreckIdentity {
    let entry = &s.npc_wrecks.roots[&root];
    NpcWreckIdentity { incarnation: s.npc_wrecks.incarnation.clone(), token: entry.token, root, revision: entry.revision }
}
fn identity(char_id: i64) -> Identity {
    Identity { char_id, ship_id: Some(SHIP), corp_id: 1, station_id: None, system_id: Some(SYSTEM_ID) }
}
fn recovery(s: &mut System, attempt: RecoveryAttempt) -> NpcWreckAccess {
    NpcWreckAccess::Recovery { character: CHAR, ship: SHIP, source: SHIP,
        source_token: s.capture_ball(SHIP).unwrap(), range: 1_000_000.0, attempt }
}
fn module(master: i64) -> RecoveryAttempt {
    RecoveryAttempt::Module { master, stop: Arc::new(std::sync::atomic::AtomicBool::new(false)) }
}
fn salvage(root: i64) -> NpcWreckOperation {
    NpcWreckOperation::Salvage { character: CHAR, destination: SHIP, yields: vec![(LOOT_TYPE, 2)],
        event: GameEvent::WreckSalvaged { char_id: CHAR, wreck_id: root, wreck_type: WRECK_TYPE, occurred_at: 0,
            context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(SHIP),
                ship_type_id: Some(1), system_id: Some(SYSTEM_ID), station_id: None, origin: FactOrigin::Natural } } }
}
fn claim(s: &mut System, root: i64, attempt: RecoveryAttempt) -> NpcWreckGuard {
    let expected = expected(s, root);
    let access = recovery(s, attempt);
    match s.claim_npc_wreck(&expected, access, salvage(root)) {
        NpcWreckLookup::Ready(guard) => guard,
        _ => panic!("claim must be admitted"),
    }
}
/// A registered-claim lookalike whose expected identity is adjusted by one
/// field: the slot must never answer a stale serial/token/revision/epoch.
fn stale_claim(s: &mut System, root: i64, adjust: impl FnOnce(&mut NpcWreckIdentity, &mut i64)) -> Arc<NpcWreckClaim> {
    let (mut identity, mut serial) = (expected(s, root), 1);
    adjust(&mut identity, &mut serial);
    Arc::new(NpcWreckClaim { system: SYSTEM_ID, root, serial, expected: identity,
        access: recovery(s, module(2)), operation: salvage(root),
        taken: None, replacement: None, receipt: None, prepared: true })
}

#[test]
fn one_claim_serializes_and_dispositions_apply_once() {
    let mut s = system();
    let root = spawn(&mut s, &[10], &[]);
    let root_expected = expected(&s, root);
    let attempt = module(1);
    let guard = claim(&mut s, root, attempt.clone());
    // A second writer and a content mutation only see the live claim; a
    // same-id ball insert is refused instead of silently replacing it.
    let foreign_access = recovery(&mut s, module(2));
    match s.claim_npc_wreck(&root_expected, foreign_access, salvage(root)) {
        NpcWreckLookup::Busy(Some(other)) => assert!(other.same_attempt(guard.claim())),
        _ => panic!("contested root must be busy"),
    }
    // A separate claimed two-stack root refuses a real conserving merge.
    let merge_root = spawn(&mut s, &[30, 31, 32], &[(LOOT_TYPE, 2), (LOOT_TYPE, 3)]);
    for item in &mut s.npc_wrecks.roots.get_mut(&merge_root).unwrap().items { item.owner_id = CHAR; }
    let merge_expected = expected(&s, merge_root);
    let [a, b] = [s.npc_wrecks.roots[&merge_root].items[0].clone(), s.npc_wrecks.roots[&merge_root].items[1].clone()];
    let mut changes = std::collections::BTreeMap::new();
    changes.insert(a.item_id, QuantityChange::new(&a, Some(a.quantity + b.quantity)));
    changes.insert(b.item_id, QuantityChange::new(&b, None));
    let merge_guard = match s.claim_npc_wreck(&merge_expected, NpcWreckAccess::GmCleanup { character: CHAR }, NpcWreckOperation::Delete { items: vec![a.item_id, b.item_id], persistent_roots: Vec::new(), persistent: Vec::new() }) {
        NpcWreckLookup::Ready(guard) => guard,
        _ => panic!("claim must be admitted"),
    };
    assert!(merge_guard.claim().prepared());
    let blocked = s.mutate_npc_loot(&identity(CHAR), &merge_expected, NpcLootMutation::Quantities(changes));
    match blocked.unwrap_err() {
        CallError::User { msg, dict } => {
            assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), marshal::Value::unicode(messages::fitting::CHARGES_SETTLING))]);
        }
        _ => panic!("busy refusal must be the custom notify"),
    };
    assert!(s.finish_npc_wreck(merge_guard.claim(), NpcWreckDisposition::RolledBack));
    assert_eq!(s.npc_wrecks.roots[&merge_root].items.iter().map(|item| item.quantity).collect::<Vec<_>>(), vec![2, 3]);
    s.insert(ball(root, None, 50.0));
    assert_eq!(s.ball(root).unwrap().pos, [0.0; 3]);
    // The attempt identity is the activation, never a same-value boolean.
    assert!(!attempt.same_attempt(&module(1)));
    assert!(!attempt.same_attempt(&RecoveryAttempt::Drone { item: 9, generation: 1 }));
    assert!(attempt.same_attempt(&attempt.clone()));
    assert!(s.finish_npc_wreck(guard.claim(), NpcWreckDisposition::RolledBack));
    assert!(s.npc_wrecks.roots[&root].claim.is_none());
    // Committed once: the empty-wreck plan applies its frozen delta once.
    let guard = claim(&mut s, root, module(3));
    assert!(s.finish_npc_wreck(guard.claim(), NpcWreckDisposition::Committed));
    assert!(!s.finish_npc_wreck(guard.claim(), NpcWreckDisposition::Committed));
    assert!(!s.npc_wrecks.roots.contains_key(&root) && s.ball(root).is_none());
    // A looted root without spare ids can only park; rollback keeps the list.
    let looted = spawn(&mut s, &[20, 21], &[(LOOT_TYPE, 5)]);
    let guard = claim(&mut s, looted, attempt);
    assert!(!guard.claim().prepared());
    assert_eq!(s.npc_wreck_claim_status(guard.claim()), Some(NpcWreckClaimState::WaitingIds));
    assert!(s.finish_npc_wreck(guard.claim(), NpcWreckDisposition::RolledBack));
    assert_eq!(s.npc_wrecks.roots[&looted].items[0].quantity, 5);
}

#[test]
fn sealed_and_parked_states_reject_foreign_and_stale_identities() {
    let mut s = system();
    let root = spawn(&mut s, &[10, 11], &[(LOOT_TYPE, 5)]);
    let guard = claim(&mut s, root, module(1));
    let sealed = guard.claim().clone();
    assert!(s.finish_npc_wreck(&sealed, NpcWreckDisposition::Unknown));
    assert_eq!(s.npc_wreck_claim_status(&sealed), Some(NpcWreckClaimState::Unknown));
    // Sealed: no view, no second writer, no impersonated finish. Each stale
    // claim differs from the registered one in exactly one identity field.
    assert!(matches!(s.npc_wreck_view(&identity(CHAR), root), NpcWreckLookup::Busy(_)));
    for (label, which) in [("serial", 0), ("token", 1), ("revision", 2), ("epoch", 3)] {
        let foreign = stale_claim(&mut s, root, |identity, serial| match which {
            0 => *serial = 99,
            1 => identity.token = BallToken::new(root, 9),
            2 => identity.revision += 1,
            _ => identity.incarnation = Arc::new(()),
        });
        assert_eq!(s.npc_wreck_claim_status(&foreign), None, "{label}");
        assert!(!s.finish_npc_wreck(&foreign, NpcWreckDisposition::RolledBack), "{label}");
    }
    // ball keeps the claim blocking through its tombstone.
    let looted = spawn(&mut s, &[30, 31], &[(LOOT_TYPE, 3)]);
    let guard = claim(&mut s, looted, module(1));
    let parked = guard.claim().clone();
    assert_eq!(s.npc_wreck_claim_status(&parked), Some(NpcWreckClaimState::WaitingIds));
    let access = recovery(&mut s, module(2));
    assert!(matches!(s.resume_npc_wreck(&parked, access), NpcWreckLookup::Denied));
    let access = recovery(&mut s, RecoveryAttempt::Drone { item: 2, generation: 3 });
    assert!(matches!(s.resume_npc_wreck(&parked, access), NpcWreckLookup::Denied));
    guard.park();
    assert!(s.cancel_waiting_npc_wreck(&parked));
    assert!(s.npc_wrecks.roots[&looted].claim.is_none() && s.npc_wrecks.roots[&looted].items[0].quantity == 3);
    // Unclaimed departure frees the root; a claimed one keeps the tombstone.
    let free = spawn(&mut s, &[40], &[]);
    s.remove_ball(free);
    assert!(!s.npc_wrecks.roots.contains_key(&free));
    let root = spawn(&mut s, &[41, 42], &[(LOOT_TYPE, 4)]);
    let guard = claim(&mut s, root, module(1));
    s.remove_ball(root);
    assert_eq!(s.npc_wreck_claim_status(guard.claim()), Some(NpcWreckClaimState::WaitingIds));
    assert!(s.npc_wreck_claims_pending() && !s.npc_wreck_discardable(root));
    let access = guard.claim().access().clone();
    assert!(matches!(s.resume_npc_wreck(guard.claim(), access), NpcWreckLookup::Absent));
    assert!(s.finish_npc_wreck(guard.claim(), NpcWreckDisposition::RolledBack));
    assert!(!s.npc_wrecks.roots.contains_key(&root));
}

#[test]
fn refill_delivery_requires_the_armed_serial_and_incarnation() {
    let mut s = system();
    let incarnation = s.npc_wrecks.incarnation.clone();
    s.npc_wreck_refill_delivered(Arc::new(()), 0, Some(vec![50]));
    assert!(s.npc_wreck_spawn(WRECK_TYPE, OWNER, 1, [0.0; 3], 1.0, None, vec![]).is_none());
    // Only an armed in-flight serial accepts a block; failure arms the cooldown.
    s.npc_wrecks.refill_in_flight = true;
    s.npc_wrecks.refill_serial = 7;
    s.npc_wreck_refill_delivered(incarnation.clone(), 8, Some(vec![51]));
    s.npc_wreck_refill_delivered(incarnation.clone(), 7, Some(vec![52, 53]));
    assert!(s.npc_wreck_spawn(WRECK_TYPE, OWNER, 1, [0.0; 3], 1.0, None, vec![(LOOT_TYPE, 1)]).is_some());
    s.npc_wrecks.refill_in_flight = true;
    s.npc_wrecks.refill_serial = 9;
    s.npc_wreck_refill_delivered(incarnation, 9, None);
    assert!(!s.npc_wrecks.refill_in_flight && s.npc_wrecks.refill_after > s.ms);
}

/// One canonical envelope built from the real receipt's key and event; each
/// case mutates a single field of an otherwise exact copy.
fn envelope(key: &str, event: &GameEvent, occurred_at: i64, context: &str, origin: &str) -> crate::progress::EventEnvelope {
    crate::progress::EventEnvelope {
        event_id: "fixture".to_string(), event: event.clone(),
        schema_version: crate::progress::codec::SCHEMA_VERSION, kind: event.kind().to_string(),
        payload: "{}".to_string(), occurred_at, recorded_at: 0,
        source_namespace: "business".to_string(), source_key: key.to_string(), ordinal: 0,
        context: context.to_string(), causation: None, origin: origin.to_string(),
        subject_character_id: Some(CHAR),
    }
}

#[test]
fn receipt_matches_only_a_complete_canonical_envelope() {
    let event = GameEvent::ItemsMoved { char_id: CHAR, item_id: 11, from_location_id: 10, to_location_id: SHIP, quantity: 3 };
    let receipt = NpcWreckReceipt::new("NpcLootTaken", "10/11/0/1".to_string(), event.clone());
    assert_eq!(receipt.source_key().as_str(), "NpcLootTaken:10/11/0/1");
    let key = receipt.source_key().as_str().to_string();
    let matches = |e: &crate::progress::EventEnvelope| crate::space::npc_wrecks::receipt_matches(e, &receipt).unwrap();
    assert!(matches(&envelope(&key, &event, 0, "{}", "server")));
    for (label, which) in [
        ("kind", 0), ("payload", 1), ("subject", 2), ("origin", 3), ("context", 4),
        ("namespace", 5), ("ordinal", 6), ("key", 7), ("schema", 8)] {
        let mut e = envelope(&key, &event, 0, "{}", "server");
        match which {
            0 => e.kind = "Other".to_string(),
            1 => e.event = GameEvent::ItemsMoved { char_id: CHAR, item_id: 11, from_location_id: 10, to_location_id: SHIP, quantity: 4 },
            2 => e.subject_character_id = None,
            3 => e.origin = "client".to_string(),
            4 => e.context = "{\"x\":1}".to_string(),
            5 => e.source_namespace = "infra".to_string(),
            6 => e.ordinal = 1,
            7 => e.source_key = "NpcLootTaken:10/11/0/2".to_string(),
            _ => e.schema_version = -1,
        }
        assert!(!matches(&e), "{label}");
    }
    // Resource events additionally bind occurred_at, the serialized context and origin.
    let salvage_event = match salvage(10) { NpcWreckOperation::Salvage { event, .. } => event, _ => unreachable!() };
    let salvage_receipt = NpcWreckReceipt::new("WreckSalvaged", "10".to_string(), salvage_event.clone());
    let GameEvent::WreckSalvaged { occurred_at, context, .. } = &salvage_event else { unreachable!() };
    let context_json = serde_json::to_string(context).unwrap();
    let matches = |e: &crate::progress::EventEnvelope| crate::space::npc_wrecks::receipt_matches(e, &salvage_receipt).unwrap();
    assert!(matches(&envelope("WreckSalvaged:10", &salvage_event, *occurred_at, &context_json, context.origin.as_str())));
    for (label, e) in [
        ("occurred_at drift", envelope("WreckSalvaged:10", &salvage_event, occurred_at + 1, &context_json, context.origin.as_str())),
        ("context stripped", envelope("WreckSalvaged:10", &salvage_event, *occurred_at, "{}", context.origin.as_str())),
        ("origin drift", envelope("WreckSalvaged:10", &salvage_event, *occurred_at, &context_json, "server")),
    ] { assert!(!matches(&e), "{label}"); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn parked_attempt_resumes_after_refill_and_installs_the_can() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let mut s = system();
    s.projection_sde = Some(sde);
    let root = spawn(&mut s, &[10, 11], &[(LOOT_TYPE, 5)]);
    let attempt = module(1);
    let guard = claim(&mut s, root, attempt.clone());
    assert_eq!(s.npc_wreck_claim_status(guard.claim()), Some(NpcWreckClaimState::WaitingIds));
    let parked = guard.claim().clone();
    guard.park();
    s.npc_wreck_install_ids(vec![12]);
    // A different activation can neither resume nor cancel the parked draw.
    let access = recovery(&mut s, module(2));
    assert!(matches!(s.resume_npc_wreck(&parked, access), NpcWreckLookup::Denied));
    let access = recovery(&mut s, RecoveryAttempt::Drone { item: 2, generation: 3 });
    assert!(matches!(s.resume_npc_wreck(&parked, access), NpcWreckLookup::Denied));
    let access = recovery(&mut s, attempt);
    let NpcWreckLookup::Ready(resumed) = s.resume_npc_wreck(&parked, access) else { panic!("same attempt resumes") };
    assert!(resumed.claim().prepared() && resumed.claim().replacement.as_ref().is_some());
    // The stale unprepared Arc can never release the resumed active writer.
    assert!(!s.cancel_waiting_npc_wreck(&parked));
    assert!(s.finish_npc_wreck(resumed.claim(), NpcWreckDisposition::Committed));
    let can = s.npc_wrecks.roots.iter().find(|(_, e)| e.kind == NpcWreckKind::LooseLootContainer).map(|(&id, _)| id).unwrap();
    assert_eq!(s.npc_wrecks.roots[&can].items.iter().map(|i| (i.item_id, i.quantity)).collect::<Vec<_>>(), vec![(11, 5)]);
}
