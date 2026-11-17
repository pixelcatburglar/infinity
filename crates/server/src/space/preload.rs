//! GM solar-system preloading behind `/loadsys` and `/loadallsys`.
//!
//! `PreloadScope::CurrentAndNeighbors` covers the caller's captured current
//! system plus the direct destinations of that system's own stargates;
//! `PreloadScope::All` covers the complete public set exposed by the static
//! map, including systems without stargates. Both sets are restricted to the
//! map's public-system membership, and a non-public current system refuses
//! selection for the neighbors scope. Only one hop is selected: wormhole
//! connections, second hops and player structures are never included.
//!
//! A batch takes one registry snapshot when it opens: targets with a live
//! actor are counted as originally loaded and skipped without touching the
//! database, the market guard or any actor mailbox. The remaining targets are
//! then loaded sequentially through the existing `World::system` cold path, so
//! each keeps its own market-guard timing: drones and unpiloted ships are
//! restored, sovereignty is recovered and durable asteroid fields may be
//! initialized; loading is not read-only. A target whose `system` call returns
//! `Ok` counts as newly loaded whether this batch or a concurrent request
//! started its actor; a failure is logged, counted, and the batch continues.
//! `unprocessed` is zero whenever the call returns normally, and the report
//! satisfies `total == originally_loaded + newly_loaded + failed + unprocessed`.
//! The snapshot does not guarantee every counted actor is still alive when the
//! batch returns, and there is no batch transaction, cancellation or resume.

use super::World;
use crate::sde::map::Map;
use tracing::warn;

/// Which systems one GM preload batch covers.
#[derive(Clone, Copy)]
pub(crate) enum PreloadScope {
    /// The given system plus its direct public stargate destinations.
    CurrentAndNeighbors(i64),
    /// The complete public static-map set, including gateless systems.
    All,
}

/// Per-batch accounting. `total == originally_loaded + newly_loaded + failed + unprocessed`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PreloadReport {
    /// Selected target count.
    pub(crate) total: i64,
    /// Targets whose actor was already live in the batch-opening snapshot.
    pub(crate) originally_loaded: i64,
    /// Targets confirmed loaded when their turn returned, including systems a
    /// concurrent request finished loading first.
    pub(crate) newly_loaded: i64,
    /// Targets whose load returned an error.
    pub(crate) failed: i64,
    /// Targets never attempted; zero when the batch returned normally.
    pub(crate) unprocessed: i64,
}

/// Sorted, deduplicated target set. `None` only when the neighbors scope's
/// current system is not a public map member; `All` is `Some` even when empty.
fn targets(map: &Map, scope: PreloadScope) -> Option<Vec<i64>> {
    let mut targets: Vec<i64> = match scope {
        PreloadScope::All => map.public_systems().map(|s| s.solar_system_id).collect(),
        PreloadScope::CurrentAndNeighbors(current) => {
            if !map.is_public_system(current) { return None; }
            let mut targets = vec![current];
            targets.extend(map.celestials(current)
                .filter_map(|c| map.stargate(c.item_id))
                .filter(|g| g.solar_system_id == Some(current))
                .filter_map(|g| g.dest_solar_system_id)
                .filter(|&dest| map.is_public_system(dest)));
            targets
        }
    };
    targets.sort_unstable();
    targets.dedup();
    Some(targets)
}

impl World {
    /// Load the scope's systems one at a time through the existing cold path
    /// and report the batch-opening snapshot plus per-target outcomes. `None`
    /// means the scope itself was refused; per-target load failures stay
    /// inside the report instead of aborting the batch.
    pub(crate) async fn preload_systems(&self, scope: PreloadScope) -> Option<PreloadReport> {
        let mut pending = targets(&self.sde.map, scope)?;
        let total = pending.len() as i64;
        // The batch-opening snapshot is one registry critical section: live
        // actors are counted once here and dropped from the pending list, with
        // no await, database or market lock inside the section.
        let originally_loaded = {
            let systems = crate::poison::lock_or_exit(self.systems.lock());
            let before = pending.len();
            pending.retain(|id| systems.get(id).is_none_or(|h| h.is_closed()));
            (before - pending.len()) as i64
        };
        let mut report = PreloadReport {
            total,
            originally_loaded,
            unprocessed: pending.len() as i64,
            ..PreloadReport::default()
        };
        for id in pending {
            match self.system(id).await {
                Ok(()) => report.newly_loaded += 1,
                Err(error) => {
                    report.failed += 1;
                    warn!(system = id, %error, "system preload failed");
                }
            }
            report.unprocessed -= 1;
        }
        Some(report)
    }
}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
