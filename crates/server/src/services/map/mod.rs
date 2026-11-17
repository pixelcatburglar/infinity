//! Routing for persistent map history and personal visits consumed by
//! `eve/client/script/ui/shared/mapView/mapViewColorHandler.py` and
//! `eve/client/script/ui/shared/mapView/filters/mapFilterVisited.py`.
//! The map telemetry DAO reads `game.map_events` and `game.map_visits`; visits
//! record successful arrivals and persist independently of history retention.
//! World/actor-to-database crash gaps can lose observations. Topology, cynos,
//! faction warfare and sovereignty are not supported by this telemetry.
mod map;
pub(crate) mod events;
pub(crate) mod retention;
#[cfg(all(test, feature = "tests-content"))] mod tests;

use crate::client::Client;
use super::{Call, CallResult};

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    map::dispatch(c, call).await
}
