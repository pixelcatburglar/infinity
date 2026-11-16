//! GM bulk standing raise: the `/maxagentstandings` (alias `/fullstandings`)
//! business operation. Enumerates the corporations and factions of live SDE
//! agents, then writes every standing to +10 through the shared mutation writer
//! in one transaction so values, history and notifications commit together.

use super::mutation::{apply_tx, Cause, Change, Mutation, Operation};
use super::{db_err, notify_error, CallError, STANDING_LIMIT};
use crate::db::outbox::{dispatch_world, Outbox};
use crate::db::Db;
use crate::space::World;
use crate::sde::Sde;
use infinity_core::messages;

/// What one `/maxagentstandings` run did, for the GM reply.
pub(crate) struct AgentStandingSummary {
    /// Unique live-agent corporation count.
    pub corporations: i64,
    /// Unique live-agent faction count.
    pub factions: i64,
    /// Owners whose standing actually changed this run.
    pub changed: i64,
    /// Owners already at +10; their rows, history and notifications are skipped.
    pub already_full: i64,
}

/// Set every live-agent corporation and faction standing of `character` to +10.
/// `actor` is the GM running the command; it becomes the history actor.
pub(crate) async fn max_agent_standings(
    db: &Db,
    world: &World,
    sde: &Sde,
    actor: i64,
    character: i64,
) -> Result<AgentStandingSummary, CallError> {
    let corporations = sde.characters.agent_corporation_ids();
    let mut factions: Vec<i64> = corporations.iter()
        .filter_map(|&id| sde.npc_corporations.get(id).and_then(|corp| corp.faction_id))
        .filter(|&id| id > 0)
        .collect();
    factions.sort_unstable();
    factions.dedup();
    let mut owners: Vec<i64> = corporations.to_vec();
    owners.extend_from_slice(&factions);
    owners.sort_unstable();
    owners.dedup();
    if owners.is_empty() {
        return Err(notify_error(messages::standings::NO_AGENT_OWNERS));
    }
    // The transaction runner may rerun the closure; an Arc hands each attempt a
    // cheap handle to the one immutable change list.
    let changes = std::sync::Arc::new(owners.iter().map(|&from_id| Change { from_id, operation: Operation::Set(STANDING_LIMIT) }).collect::<Vec<Change>>());
    let occurred_at = crate::client::filetime_now();
    let changed = db.transaction(
        |tx, outbox: &mut Outbox| {
            let changes = changes.clone();
            Box::pin(async move {
                apply_tx(tx, outbox, character, &changes, Mutation { occurred_at, cause: Cause::Slash { actor, reason: "" } }).await
            })
        },
        |changed, outbox| async move { dispatch_world(changed, outbox, world) },
    ).await?;
    Ok(AgentStandingSummary {
        corporations: i64::try_from(corporations.len()).map_err(db_err)?,
        factions: i64::try_from(factions.len()).map_err(db_err)?,
        changed,
        already_full: i64::try_from(owners.len()).map_err(db_err)? - changed,
    })
}
