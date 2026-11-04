//! Empty victory point state for `fwVictoryPointMgr`.
//! `eve/client/script/ui/services/fwVictoryPointSvc.py` caches the current
//! session system; no occupation or points are modelled.
//! No persistent data is used.

use super::{Call, CallResult};
use crate::client::Client;
use marshal::Value;

pub(super) fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match (call.service, call.method) {
        ("fwVictoryPointMgr", "GetLocalVictoryPointState") => Value::tuple([c.session.get("solarsystemid2").clone(), Value::None]),
        ("fwVictoryPointMgr", "GetAllVictoryPointStates") => Value::Dict(vec![]),
        _ => return None,
    }))
}

#[cfg(test)]
mod tests;
