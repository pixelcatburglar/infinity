//! First-party `shipStanceMgr.SetShipStance(shipID, stanceID)` used by client
//! `eve/client/script/ui/inflight/shipstance.py:26-32`; the result is unused,
//! and fitting/station views reach the same call. The switch mechanic lives in
//! `ship_stance::switch`; there is no corporation role gate.
use infinity_core::messages;
use marshal::Value;
use crate::client::Client;
use crate::services::{identity::Identity, notify_error, ship_stance::switch, Call, CallResult};

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "shipStanceMgr" || call.method != "SetShipStance" { return None; }
    Some(match call.args {
        [Value::Int(ship), Value::Int(stance)] if *ship > 0 && *stance > 0 => {
            switch::set_stance(&Identity::of_client(c), &c.db, &c.world, &c.sde, c.login.user_id, *ship, *stance,
                |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await
        }
        _ => Err(notify_error(messages::ship_stance::INVALID_REQUEST)),
    })
}

#[cfg(all(test, feature = "tests-services"))]
mod tests;
