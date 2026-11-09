//! Read-only crate directories for crate/controller.py and
//! recruitment_crate_controller.py. Definitions come from the startup SDE
//! catalogue; item access uses game.items and inventory reachability. Opening,
//! reward selection and inventory changes are not supported.
//! Wire preview quantities are projected to the client display limit;
//! catalogue definitions stay exact.

use crate::client::Client;
use infinity_core::consts::FLAG_CORP_DELIVERIES;
use crate::dao::{items::loading::ItemQuery, skills::training};
use infinity_core::messages::crates as messages;
use crate::services::{db_err, notify_error, CallError, CallResult};
use crate::services::identity::Identity;
use crate::services::inventory::access::{corporate_custody, may_view_corp_hangar};
use crate::services::inventory::reachability::{Endpoint, Reachability};
use crate::services::items::load_items;
use marshal::Value;

fn invalid() -> CallError { notify_error(messages::INVALID_SOURCE) }

async fn race(c: &Client, identity: &Identity) -> Result<Option<i32>, CallError> {
    if identity.char_id == 0 { return Ok(None); }
    training::race(&c.db, identity.char_id).await.map_err(db_err)?
        .map(Some).ok_or_else(invalid)
}

pub(super) async fn directory(c: &Client, type_id: i32, custom: bool, identity: &Identity) -> CallResult {
    let definition = c.sde.crates.get(type_id).ok_or_else(invalid)?;
    let race_id = race(c, identity).await?;
    let entries = definition.preview(custom, race_id)
        .map_err(|_| notify_error(messages::METADATA_UNAVAILABLE))?;
    Ok(Value::list(entries.into_iter().map(|(kind, quantity)| {
        Value::tuple([Value::Int(i64::from(kind)), Value::Int(crate::services::dogma::info::cap_client_quantity(quantity)), Value::None])
    })))
}

pub(super) async fn instance(c: &Client, item_id: i64, custom: bool) -> CallResult {
    let identity = Identity::of_client(c);
    if identity.char_id == 0 { return Err(invalid()); }
    let _guard = crate::services::market::lock().await;
    let item = load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err)?
        .into_iter().next().ok_or_else(invalid)?;
    if let Some((corp, location, flag)) = corporate_custody(&c.db, Some(&c.sde), item.item_id).await? {
        let station = crate::services::inventory::access::custody_station(&c.db, location, flag).await?.ok_or_else(invalid)?;
        if corp != identity.corp_id || (item.owner_id != corp && item.owner_id != identity.char_id)
            || (flag == FLAG_CORP_DELIVERIES && identity.station_id != Some(location))
            || !may_view_corp_hangar(&c.db, identity.char_id, corp, station, flag).await? {
            return Err(invalid());
        }
    } else if item.owner_id != identity.char_id {
        return Err(invalid());
    }
    let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
    let reach = Reachability::capture(&identity, &c.db, &c.world, c.session.int("solarsystemid"), &[endpoint]).await?;
    reach.check(&c.db, &c.sde, endpoint).await?;
    directory(c, item.type_id, custom, &identity).await
}
