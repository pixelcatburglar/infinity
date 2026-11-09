//! Station and login services, including station data used by `uisvc.GetStation`.
//! Station information comes from the startup SDE map; activity auto-popup state is
//! persisted per character in `game.characters` (missing characters default to 1).
//! Activity badges and other unmodelled services return empty or no-op fallbacks.
//! The SettingsInfo code object used by carbonui settingsSvc.UpdateSettingsStatistics
//! is CPython 2.7 marshal and bytecode, not portable to another Python version.

use super::{db_err, rowset, Call, CallError, CallResult};
use crate::client::Client;
use marshal::Value;




/// `Some(reply)` for calls this module answers, `None` to fall through.
mod insurgency_solarsystem;
mod fw_warzone_solarsystem;
mod map;
mod activity_mgr;
mod station_svc;
mod jump_timers;
mod macho_net;
mod chat_authentication_service;

pub(super) async fn root(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "machoNet" => macho_net::dispatch(c, call).await,
        "chatAuthenticationService" => chat_authentication_service::dispatch(c, call).await,
        _ => None,
    }
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if let Some(reply) = super::fixed::station(call) { return Some(reply); }
    match call.service {
        "insurgencySolarsystem" => insurgency_solarsystem::dispatch(c, call).await,
        "fwWarzoneSolarsystem" => fw_warzone_solarsystem::dispatch(c, call).await,
        "map" => map::dispatch(c, call).await,
        "activityMgr" => activity_mgr::dispatch(c, call).await,
        "stationSvc" => station_svc::dispatch(c, call).await,
        "jumpTimers" => jump_timers::dispatch(c, call).await,
        _ => None,
    }
}
