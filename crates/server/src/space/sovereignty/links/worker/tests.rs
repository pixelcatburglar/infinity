//! Actor callback reconciliation tests; these do not exercise database transactions.
use super::*;
use crate::{dao::sovereignty::capture::CaptureRow, services::sovereignty::CaptureContributor};

fn row() -> CaptureRow {
    CaptureRow { target_item_id: 1, system_id: 1, ownership_epoch: 1, controlling_team: None,
        scoring_team: None, defender_team: 0, score: 0.0, rate: 0.0, blocked: false,
        score_sim_time: filetime_from_unix_ms(0), checkpoint_wall_time: 0, revision: 1,
        capture_time_seconds: 10.0, defensive_regen_rate: 0.0, checkpoint_interval_ms: 1000,
        content_namespace: "test".into(), content_version: "test".into() }
}
fn setup() -> System {
    let mut system = System::default();
    system.sovereignty.version = Some((1, 2));
    system.sovereignty.links.anchor = Some(row());
    system.sovereignty.links.current = Some(row());
    system.sovereignty.links.dirty = true;
    system.sovereignty.links.inflight = Some(Inflight { token: 5, target_item_id: 1, epoch: 1, system_revision: 2, revision: 1, through: row().score_sim_time });
    system
}
#[test]
fn stale_token_and_lifecycle_cannot_acknowledge() {
    let mut system = setup();
    complete(&mut system, 4, Reply::Error);
    assert!(system.sovereignty.links.inflight.is_some());
    system.sovereignty.version = Some((2, 3));
    complete(&mut system, 5, Reply::Error);
    assert!(system.sovereignty.links.inflight.is_some());
}
#[test]
fn error_retains_dirty_and_frozen_and_close_cancels_work() {
    let mut system = setup();
    let observation = replay::observation(&system.sovereignty.links, row(), 2, None);
    system.sovereignty.links.frozen = Some(observation);
    complete(&mut system, 5, Reply::Error);
    assert!(system.sovereignty.links.dirty);
    assert!(system.sovereignty.links.frozen.is_some());
    close(&mut system);
    assert!(system.sovereignty.links.inflight.is_none());
    assert!(system.sovereignty.links.frozen.is_some());
}
#[test]
fn accepted_checkpoint_retains_newer_paid_interval() {
    let mut system = setup();
    let start = row().score_sim_time;
    let second = infinity_core::consts::FILETIME_SECOND;
    let contributor = CaptureContributor { character_id: 1, ship_id: 1, module_id: 1,
        corporation_id: 1, alliance_id: 1, source_revision: 1 };
    system.sovereignty.links.segments.push(super::super::Segment { contributor: contributor.clone(), start, end: start + second });
    system.sovereignty.links.segments.push(super::super::Segment { contributor, start: start + second, end: start + 3 * second });
    system.sovereignty.links.pressure = true;
    system.sovereignty.links.inflight.as_mut().unwrap().through = start + second;
    let mut accepted = row();
    accepted.revision = 2;
    accepted.score_sim_time = start + second;
    complete(&mut system, 5, Reply::Checkpoint(CheckpointOutcome::Accepted(accepted)));
    assert_eq!(system.sovereignty.links.anchor.as_ref().unwrap().revision, 2);
    assert_eq!(system.sovereignty.links.anchor.as_ref().unwrap().score_sim_time, start + second);
    assert_eq!(system.sovereignty.links.segments.len(), 1);
    assert_eq!(system.sovereignty.links.segments[0].end, start + 3 * second);
    assert!(!system.sovereignty.links.pressure);
    assert!(system.sovereignty.links.dirty);
}

#[test]
fn rejected_proof_keeps_pressure_and_unacknowledged_prefix() {
    use crate::dao::sovereignty::{sources::SourceChange, systems::SystemRow};
    let mut system = setup();
    let start = row().score_sim_time;
    let second = infinity_core::consts::FILETIME_SECOND;
    system.sovereignty.links.segments.push(super::super::Segment {
        contributor: CaptureContributor { character_id: 1, ship_id: 1, module_id: 1,
            corporation_id: 1, alliance_id: 1, source_revision: 0 },
        start, end: start + 3 * second,
    });
    system.sovereignty.links.pressure = true;
    let current = HubPublication { system: SystemRow { system_id: 1, hub_item_id: Some(1),
        claim_corporation_id: None, alliance_id: None, claim_time: None,
        ownership_epoch: 1, revision: 2 }, hub: None, capture: Some(row()) };
    complete(&mut system, 5, Reply::Checkpoint(CheckpointOutcome::Rejected {
        current, changes: vec![SourceChange { character_id: 1, revision: 1, effective_at: start + second }],
    }));
    assert!(system.sovereignty.links.pressure);
    assert_eq!(system.sovereignty.links.anchor.as_ref().unwrap().score_sim_time, start);
    assert_eq!(system.sovereignty.links.segments.len(), 1);
    assert_eq!(system.sovereignty.links.segments[0].end, start + second);
    assert!(system.sovereignty.links.dirty);
}
