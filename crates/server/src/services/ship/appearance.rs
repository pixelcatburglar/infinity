//! Physical ship-hull appearance shared by the cleaning and kill-counter
//! RPCs, cold ball preparation and GM writes. `game.ship_dirt` persists the
//! timestamp and `game.ship_kill_counts` the player-kill count through
//! `items::ship_appearance`, `items::ship_dirt` and `items::ship_kill_counts`;
//! a committed change reaches the owning system actor after commit as an
//! `OnSlimItemChange` slim patch. The count follows the hull and is never
//! attributed automatically. Mirrors `cleanShipButton.py`, the
//! `hangarBehaviours` GetDirtTimestamp readers, the `shipKillCounter` protocol
//! and the `playerShip.py` slim `dirtTime`/`kills` consumers. A missing dirt
//! row is initialized only by the cleaning read or cold prepare, never by
//! status inspection; a missing count row reads as 0 and is never initialized.
//! No wallet charge, dirt growth timer or dedicated client notification is
//! modeled; the client refreshes hangar dirt locally after its own reset call.
pub(crate) mod dirt;
pub(crate) mod gm;

use infinity_core::consts::CATEGORY_SHIP;
use crate::dao::items::{ship_appearance::{self, ObservedHull}, ship_dirt, ship_kill_counts};
use crate::db::{executor::Executor, outbox::{self, Outbox}};
use crate::sde::Sde;
use crate::services::{db_err, CallError};
use crate::space::{system::appearance::{Fence, Patch}, World};
use marshal::Value;

/// Type, verbatim item name, dirt timestamp and player-kill count carried by
/// a prepared hull's slim item; the builder owns the NULL-name SDE fallback.
pub(crate) struct HullAppearance {
    pub type_id: i32,
    pub name: Option<String>,
    pub dirt_timestamp: i64,
    /// Persisted player-kill count; a missing `game.ship_kill_counts` row is 0.
    pub player_kills: i64,
}

/// A committed appearance write awaiting live publication after commit.
pub(crate) struct AppearanceChanged {
    pub fence: Option<Fence>,
    pub patch: Patch,
}
crate::progress::leaf_effect!(AppearanceChanged);

/// How the committed write reached the live ball, when a fence was captured.
/// `Stale` means the database kept the value but the captured incarnation was
/// gone at publish time; a later real prepare recovers the stored timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Publication { NotLoaded, Published, Stale }

/// Load the physical hull's appearance for one ball being prepared.
/// A stored timestamp is used as-is; a missing row is initialized once at
/// `now` under the custody guard, so a cold hull starts aging from its first
/// real load instead of reporting a fresh `now` forever. The kill count comes
/// from the same joined read and a missing row reports 0.
pub(crate) async fn prepare(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, now: i64) -> Result<HullAppearance, CallError> {
    let stored = ship_appearance::load(ex, ship).await.map_err(db_err)?
        .filter(|stored| sde.types.get(stored.hull.type_id).and_then(|ty| ty.category_id) == Some(CATEGORY_SHIP))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("item {ship} is not a physical ship hull")))?;
    let dirt_timestamp = match stored.dirt_timestamp {
        Some(stamp) => stamp,
        None => ship_dirt::get_or_init(ex, &stored.hull, now).await.map_err(db_err)?
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("ship {ship} lost its physical custody during dirt initialization")))?,
    };
    Ok(HullAppearance { type_id: stored.hull.type_id, name: stored.name, dirt_timestamp, player_kills: stored.player_kills })
}

/// A captured fence must name this hull's ship, system, owner and type, so a
/// row write and its live patch can never carry different identities.
fn fence_matches(hull: &ObservedHull, fence: Option<&Fence>) -> Result<(), CallError> {
    if let Some(fence) = fence {
        if fence.ship != hull.item_id || fence.system != hull.location_id
            || fence.owner != hull.owner_id || fence.type_id != hull.type_id {
            return Err(crate::services::notify_error(infinity_core::messages::ship::HULL_APPEARANCE_INACCESSIBLE));
        }
    }
    Ok(())
}

/// Persist the timestamp on an already locked and authorized hull and queue
/// the live patch for post-commit publication. The caller holds the hull row
/// lock until commit; nothing reaches the actor when the transaction rolls back.
pub(crate) async fn set_dirt_tx(
    ex: &(impl Executor + ?Sized), hull: &ObservedHull, stamp: i64,
    fence: Option<&Fence>, outbox: &mut Outbox<AppearanceChanged>,
) -> Result<i64, CallError> {
    fence_matches(hull, fence)?;
    let stamp = ship_dirt::set_timestamp(ex, hull.item_id, stamp).await.map_err(db_err)?;
    outbox.operation(AppearanceChanged { fence: fence.cloned(), patch: Patch { dirt_timestamp: Some(stamp), player_kills: None } });
    Ok(stamp)
}

/// Persist the player-kill count under the same guard and publication
/// contract as `set_dirt_tx`; only `ship_kill_counts` is written and only the
/// `kills` slim field is patched, so a count write never touches dirt.
async fn set_kills_tx(
    ex: &(impl Executor + ?Sized), hull: &ObservedHull, count: i64,
    fence: Option<&Fence>, outbox: &mut Outbox<AppearanceChanged>,
) -> Result<i64, CallError> {
    fence_matches(hull, fence)?;
    let count = ship_kill_counts::set_player_kills(ex, hull.item_id, count).await.map_err(db_err)?;
    outbox.operation(AppearanceChanged { fence: fence.cloned(), patch: Patch { dirt_timestamp: None, player_kills: Some(count) } });
    Ok(count)
}

/// Encode a dirt timestamp so the wire value is a Python `long`, which
/// `blue.os.TimeDiffInMs` requires. Zero uses the canonical empty LONG payload,
/// a nonzero value inside the i32 domain uses the shortest signed little-endian
/// payload, and anything outside that domain reuses the codec's own LONG
/// encoding of `Value::Int` without an extra payload allocation.
pub(crate) fn dirt_timestamp_value(timestamp: i64) -> Value {
    let Ok(value) = i32::try_from(timestamp) else { return Value::Int(timestamp) };
    if value == 0 { return Value::BigLong(Vec::new()); }
    let bytes = value.to_le_bytes();
    let bits = i32::BITS - if value > 0 { value.leading_zeros() } else { (!value).leading_zeros() };
    let len = ((bits + 1) as usize).div_ceil(u8::BITS as usize);
    Value::BigLong(bytes[..len].to_vec())
}

/// Post-commit dispatcher for a transaction carrying appearance operations.
/// Each fenced operation gets one awaited actor turn; `Stale` reports that the
/// captured incarnation was gone or rejected the patch while the committed row
/// stands. `NotLoaded` is only for a hull that never had a fence to publish to.
pub(crate) async fn dispatch_changed<T>(value: T, outbox: Outbox<AppearanceChanged>, world: &World)
    -> Result<(T, Publication), CallError> {
    let mut publication = Publication::NotLoaded;
    for entry in outbox.into_entries() {
        match entry {
            outbox::Effect::Operation(AppearanceChanged { fence, patch }) => {
                let next = match fence {
                    Some(fence) => match world.with(fence.system, move |s| s.publish_appearance(&fence, patch)).await {
                        Some(true) => Publication::Published,
                        _ => Publication::Stale,
                    },
                    None => Publication::NotLoaded,
                };
                publication = match (publication, next) {
                    (Publication::Stale, _) | (_, Publication::Stale) => Publication::Stale,
                    (Publication::Published, _) | (_, Publication::Published) => Publication::Published,
                    _ => Publication::NotLoaded,
                };
            }
            outbox::Effect::Notification(notification) => {
                if let Err(error) = notification.dispatch(world, None) { tracing::warn!("postcommit notification failed: {error:?}"); }
            }
        }
    }
    Ok((value, publication))
}

#[cfg(all(test, feature = "tests-ships"))]
mod tests;
