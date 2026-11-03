//! Character-scoped services once in game, including paperdolls, assets, repairs and private notes.
//! Client paths include `charMgr`, `paperDollServer`, `repairSvc` and related services.
//! Data comes from character and item DAOs and the SDE; unmodelled systems return empty answers.
use super::{db_err, keyval, rowset, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{dogma::ATTR_STRUCTURE_HP, FILETIME_SECOND, FILETIME_UNIX_EPOCH, TYPE_CHARACTER};
use marshal::Value;

mod char_mgr;
mod contacts;
mod paper_doll_server;
mod corporation_svc;
mod repair_svc;
pub(super) mod home_station;
mod account;
mod station;
pub(super) mod provisioning;
pub(crate) mod creation;

fn char_id(c: &Client) -> i64 { c.session.int("charid").unwrap_or(0) }

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "charMgr" => char_mgr::dispatch(c, call).await,
        "paperDollServer" => paper_doll_server::dispatch(c, call).await,
        "corporationSvc" => corporation_svc::dispatch(c, call).await,
        "repairSvc" => repair_svc::dispatch(c, call).await,
        "home_station" => home_station::dispatch(c, call).await,
        "account" => account::dispatch(c, call).await,
        "station" => station::dispatch(c, call).await,
        _ => None,
    }
}
