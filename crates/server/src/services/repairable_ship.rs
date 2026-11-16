//! Repairable-ship recovery and service-life RPCs consumed by
//! `repairableShip/client/repairable_ship_svc.py`. Recovery facts, physical
//! custody, wallet debits and deadlines are persisted by their respective DAOs.
//! Server price data is captured at startup; a running client can display old
//! prices until restarted (logging out or switching characters is insufficient).
//! Charges always use the current server price. Player-loss production is
//! performed by the combat loss transaction, not by this service.

pub(crate) mod deadlines;
pub(in crate::services) mod recovery;
pub(crate) mod service_life;
pub(crate) mod wire;
mod workshop;

use crate::{client::Client, dao::repairable_ship::ItemId, services::{identity::Identity, notify_error, Call, CallResult}};
use infinity_core::messages::repairable_ship as msg;
fn refurbishment_result(rows: &[(i64, marshal::Value)], updates: &std::collections::BTreeMap<ItemId, f64>) -> marshal::Value {
    marshal::Value::Dict(rows.iter().filter_map(|(id, row)| {
        let item = ItemId::new(*id)?;
        updates.get(&item).map(|damage| (row.clone(), marshal::Value::Float(*damage)))
    }).collect())
}

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "repairableShipMgr" { return None; }
    if matches!(call.method, "CallRequested" | "CallDeliveryPurchased" | "CallAbortRequested"
        | "CallItemsWithdrawalRequested" | "GetBatchDestinationSolarSystem" | "OnDisassemble"
        | "OnRefurbishment" | "GetServiceLifeDamage") && Identity::of_client(c).char_id <= 0 {
        return Some(Err(notify_error(msg::SELECTION_INVALID)));
    }
    Some(match call.method {
        "CallRequested" => recovery::list(c).await,
        "CallDeliveryPurchased" => recovery::purchase(c, call).await,
        "CallAbortRequested" => recovery::abort(c, call).await,
        "CallItemsWithdrawalRequested" => recovery::withdraw(c, call).await,
        "GetBatchDestinationSolarSystem" => recovery::destinations(c, call).await,
        "OnDisassemble" => workshop::disassemble(c, call).await,
        "OnRefurbishment" => workshop::refurbish(c, call).await,
        "GetServiceLifeDamage" => match call.args.first().and_then(|v| v.as_int()).and_then(ItemId::new) {
            Some(item) => service_life::single(&c.db, &c.sde, &Identity::of_client(c), item).await,
            None => Err(notify_error(msg::SELECTION_INVALID)),
        },
        _ => return None,
    })
}

#[cfg(all(test, feature = "tests-services"))] mod tests;
