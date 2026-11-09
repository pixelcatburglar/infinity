//! Coalesced guarded checkpoints and frozen capture settlement on the system worker.
//! The sovereignty services own durable writes; actor callbacks reconcile their replies.
//! No work is scheduled after the readiness barrier closes.
use super::{replay, score, Inflight};
use crate::{client::filetime_from_unix_ms, services::sovereignty::{capture::{self, CheckpointOutcome}, lifecycle::{self, CaptureOutcome}, HubPublication, Resources}, space::System};

enum Reply { Checkpoint(CheckpointOutcome), Claim(CaptureOutcome), Error }

fn matches(system: &System, token: u64) -> bool {
    let state = &system.sovereignty.links;
    state.inflight.as_ref().is_some_and(|job| job.token == token
        && system.sovereignty.version == Some((job.epoch, job.system_revision))
        && state.anchor.as_ref().is_some_and(|row| row.target_item_id == job.target_item_id && row.ownership_epoch == job.epoch && row.revision == job.revision))
}

fn correction(system: &mut System, publication: HubPublication, changes: Vec<crate::dao::sovereignty::sources::SourceChange>) {
    let version = (publication.system.ownership_epoch, publication.system.revision);
    let same = system.sovereignty.version == Some(version) && publication.capture.as_ref().zip(system.sovereignty.links.anchor.as_ref()).is_some_and(|(new, old)| new.target_item_id == old.target_item_id && new.ownership_epoch == old.ownership_epoch);
    if !same {
        if let Err(error) = super::super::apply(system, publication, false, &[]) { tracing::warn!(%error, "sovereignty reconciliation failed"); }
        return;
    }
    let state = &mut system.sovereignty.links;
    let row = publication.capture.unwrap();
    if state.anchor.as_ref().is_some_and(|old| row.revision < old.revision) {
        super::invalidate(system, &changes);
        return;
    }
    let through = row.score_sim_time;
    state.anchor = Some(row);
    state.segments.retain(|segment| segment.end > through);
    state.frozen = None;
    super::invalidate(system, &changes);
    let now = filetime_from_unix_ms(system.now_ms());
    replay::boundary(system, now);
    system.sovereignty.links.dirty = true;
}

fn complete(system: &mut System, token: u64, reply: Reply) {
    if !matches(system, token) { return; }
    let job = system.sovereignty.links.inflight.take().unwrap();
    match reply {
        Reply::Error => { system.sovereignty.links.dirty = true; }
        Reply::Checkpoint(CheckpointOutcome::Accepted(row)) => {
            if row.target_item_id != job.target_item_id || row.ownership_epoch != job.epoch || row.revision <= job.revision || row.score_sim_time != job.through { system.sovereignty.links.dirty = true; return; }
            let state = &mut system.sovereignty.links;
            state.segments.retain(|segment| segment.end > job.through);
            state.anchor = Some(row);
            state.frozen = None;
            state.pressure = false;
            state.dirty = !state.segments.is_empty();
            let now = filetime_from_unix_ms(system.now_ms());
            replay::boundary(system, now);
        }
        Reply::Checkpoint(CheckpointOutcome::Rejected { current, changes }) | Reply::Claim(CaptureOutcome::Rejected { current, changes }) => correction(system, current, changes),
        Reply::Claim(CaptureOutcome::Claimed) => { system.sovereignty.links.frozen = None; }
    }
}

pub(super) fn tick(system: &mut System) {
    if !super::ready(system) { return; }
    let now_ms = system.now_ms();
    let now = filetime_from_unix_ms(now_ms);
    let state = &mut system.sovereignty.links;
    let Some(row) = state.current.as_mut() else { return };
    if state.frozen.is_none() {
        let boundary = state.segments.iter().any(|s| s.end > row.score_sim_time && s.end <= now || s.start > row.score_sim_time && s.start <= now)
            || score::capture_timestamp(row).is_some_and(|endpoint| endpoint <= now);
        if boundary { replay::boundary(system, now); }
        else {
            let controlling = row.controlling_team;
            score::advance(row, now);
            if row.controlling_team != controlling {
                let changed = row.clone();
                state.dirty = true;
                replay::publish(system, &changed);
            }
        }
    }
    let state = &mut system.sovereignty.links;
    if state.inflight.is_some() || (!state.dirty && state.frozen.is_none()) || now_ms < state.deadline_ms { return; }
    let Some((epoch, system_revision)) = system.sovereignty.version else { return };
    let Some(row) = state.current.clone() else { return };
    let claim = state.frozen.is_some();
    let observation = state.frozen.clone().unwrap_or_else(|| replay::observation(state, row, system_revision, None));
    let token = state.next_token.wrapping_add(1);
    state.next_token = token;
    state.deadline_ms = now_ms.saturating_add(observation.target.checkpoint_interval_ms.max(1));
    state.inflight = Some(Inflight { token, target_item_id: observation.target.target_item_id, epoch, system_revision, revision: observation.target.revision, through: observation.target.score_sim_time });
    let weak = system.world.clone();
    let system_id = system.id;
    if !system.try_defer(async move {
        let Some(world) = weak.upgrade() else { return };
        let resources = Resources { db: world.db().clone(), sde: world.sde().clone(), world: world.clone() };
        let reply = if claim {
            match lifecycle::claim(resources, observation).await { Ok(result) => Reply::Claim(result), Err(error) => { tracing::warn!(?error, "sovereignty claim failed"); Reply::Error } }
        } else {
            match capture::checkpoint(resources, vec![observation]).await {
                Ok(mut results) if results.len() == 1 => Reply::Checkpoint(results.remove(0)),
                Ok(_) => { tracing::warn!("invalid sovereignty checkpoint response"); Reply::Error },
                Err(error) => { tracing::warn!(?error, "sovereignty checkpoint failed"); Reply::Error }
            }
        };
        world.post(system_id, move |system| complete(system, token, reply));
    }) { system.sovereignty.links.inflight = None; system.sovereignty.links.dirty = true; }
}

pub(super) fn close(system: &mut System) {
    system.sovereignty.links.inflight = None;
}

#[cfg(test)] mod tests;
