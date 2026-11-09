//! Ships in the hangar and their fittings: assembling and repackaging
//! (the `repackagingSvc`), saved fittings
//! (`charFittingMgr`, eve/client/script/environment/fittingSvc.py) and the
//! character's blueprints (`blueprintManager`, eve/client/script/industry/blueprintSvc.py).
//! Fitting modules themselves is inventory moves into slot flags (inventory.rs) plus
//! the online state dogma.rs keeps.
//! Assembly updates packaged source hulls atomically before creating assembled hulls.
//! Assembly follows menucheckers/itemCheckers.py:93-115 and
//! eve/client/script/ui/services/menuSvcExtras/invItemFunctions.py:175-177:
//! current personal station hangar or active ship cargo in space, excluding modular ships in space.
//! Repackaging follows invItemFunctions.py:544-580 and itemCheckers.py:688-705,953-957:
//! direct personal hangars at named SDE stations, including remote stations.
//! Corporation, array and player-structure access is not modeled.


pub(super) mod ship;
pub(in crate::services) mod newbie;
mod repackaging_svc;
mod char_fitting_mgr;
mod corp_fitting_mgr;
mod alliance_fitting_mgr;
mod blueprint_manager;
use crate::client::Client;
use super::{Call, CallResult};
pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "repackagingSvc" => repackaging_svc::dispatch(c, call).await,
        "charFittingMgr" => char_fitting_mgr::dispatch(c, call).await,
        "corpFittingMgr" => corp_fitting_mgr::dispatch(call).await,
        "allianceFittingMgr" => alliance_fitting_mgr::dispatch(call).await,
        "blueprintManager" => blueprint_manager::dispatch(c, call).await,
        _ => None,
    }
}
