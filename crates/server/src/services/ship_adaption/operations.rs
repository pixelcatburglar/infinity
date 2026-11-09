//! Atomic physical-chip conversion and loyalty exchange on the canonical ledgers.
//! The caller owns market ordering and generation admission; no Lua or publication
//! gate runs while this transaction holds character, subject, item or loyalty rows.

use std::{collections::BTreeMap, sync::Arc};
use infinity_core::{consts::{FLAG_HANGAR, IX_STACK_SIZE}};
use crate::{dao::{characters, items::{loading::ItemQuery, quantities::{self, QuantityChange}}, ship_adaption, trades}, db::{Db, outbox::{self, Outbox}}, lua::{content::{adaption::{self, Definitions}, protocol::{self, Identity as WireIdentity, Response}}, events::resources::{FactContext, FactOrigin, LoyaltyReason}}, sde::Sde, services::{CallError, db_err, identity::Identity, inventory::notifications, items, notify_error, standings}, space::{NotifyTx, World}};
use marshal::Value;
use super::{policy::{self, Action, AdmittedPlan}, wire};

pub(crate) fn refusal(definitions: &Definitions, key: &str) -> CallError {
    match definitions.message(key) {
        Some(text) => notify_error(text),
        None => db_err(anyhow::anyhow!("missing adaptation refusal text {key}")),
    }
}

pub(super) fn exchange_mutation(location: &characters::ship::Location, now: i64) -> standings::LoyaltyMutation {
    standings::LoyaltyMutation {
        reason: LoyaltyReason::ChipExchange, occurred_at: now,
        context: FactContext { corporation_id: Some(location.corporation), alliance_id: location.alliance, faction_id: None, ship_id: location.ship, ship_type_id: None, system_id: location.system, station_id: location.station, origin: FactOrigin::Natural },
    }
}

pub(crate) async fn commit(db: &Db, world: &World, sender: &NotifyTx, sde: Arc<Sde>, identity: Identity, user: i64, expected: i64, plan: Arc<AdmittedPlan>, item_id: Option<i64>) -> Result<(), CallError> {
    let now = crate::client::filetime_now();
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let plan = plan.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let policy = plan.policy();
                let definitions = policy.adaption().ok_or_else(|| db_err(anyhow::anyhow!("admitted policy is not adaptation")))?;
                let version = policy.version();
                let character = identity.char_id;
                characters::selection::lock_character(tx, character).await.map_err(db_err)?;
                let location = characters::ship::current(tx, character).await.map_err(db_err)?
                    .ok_or_else(|| refusal(definitions, adaption::INVALID))?;
                if !characters::selection::owns(tx, character, user).await.map_err(db_err)? {
                    return Err(refusal(definitions, adaption::INVALID));
                }
                let subject = ship_adaption::lock_subject(tx, character, version).await.map_err(db_err)?;
                if subject.revision != expected { return Err(refusal(definitions, adaption::CONFLICT)); }
                match plan.action() {
                    Action::Inject { type_id, quantity, credit_type, credit_quantity } => {
                        let item_id = item_id.ok_or_else(|| refusal(definitions, adaption::ITEM))?;
                        let station = location.station.filter(|station| sde.map.station(*station).is_some())
                            .filter(|station| identity.station_id == Some(*station))
                            .ok_or_else(|| refusal(definitions, adaption::ITEM))?;
                        if !trades::reserved(tx, &[item_id]).await.map_err(db_err)?.is_empty() { return Err(refusal(definitions, adaption::ITEM)); }
                        let item = items::load_items(tx, &sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop()
                            .ok_or_else(|| refusal(definitions, adaption::ITEM))?;
                        let amount = i64::from(*quantity);
                        if item.owner_id != character || item.location_id != station || item.flag_id != FLAG_HANGAR || item.singleton
                            || item.type_id != *type_id || item.quantity < amount || amount <= 0
                            || policy::conversion(&sde, definitions, item.type_id, *quantity) != Some((*credit_type, *credit_quantity)) { return Err(refusal(definitions, adaption::ITEM)); }
                        let remaining = item.quantity - amount;
                        let changes = BTreeMap::from([(item_id, QuantityChange::new(&item, (remaining > 0).then_some(remaining)))]);
                        if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(refusal(definitions, adaption::CONFLICT)); }
                        if !ship_adaption::chips::credit(tx, character, *credit_type, *credit_quantity).await.map_err(db_err)? { return Err(refusal(definitions, adaption::OVERFLOW)); }
                        if !ship_adaption::advance(tx, character, expected, version).await.map_err(db_err)? { return Err(refusal(definitions, adaption::CONFLICT)); }
                        let snapshot = ship_adaption::snapshot(tx, character).await.map_err(db_err)?;
                        if remaining == 0 { notifications::notify_item_removed(outbox, item); }
                        else { notifications::notify_item_change(tx, &sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?; }
                        wire::notice(outbox, "OnUserResponsed", wire::user(definitions, &snapshot, user, character)?);
                    }
                    Action::Exchange { issuer, chips, total } => {
                        for &(type_id, quantity) in chips.iter() {
                            if !ship_adaption::chips::debit(tx, character, type_id, quantity).await.map_err(db_err)? { return Err(refusal(definitions, adaption::CHIPS)); }
                        }
                        standings::add_lp(tx, outbox, character, *issuer, *total, exchange_mutation(&location, now)).await?;
                        if !ship_adaption::advance(tx, character, expected, version).await.map_err(db_err)? { return Err(refusal(definitions, adaption::CONFLICT)); }
                        let snapshot = ship_adaption::snapshot(tx, character).await.map_err(db_err)?;
                        wire::notice(outbox, "OnUserResponsed", wire::user(definitions, &snapshot, user, character)?);
                        let text = protocol::encode_content_text(Response::Exchange { identity: WireIdentity { user, character }, corporation: *issuer }).map_err(db_err)?;
                        wire::notice(outbox, "OnChipExchangeSuccessed", text);
                    }
                }
                Ok(())
            })
        },
        |value, outbox| async move { outbox::dispatch_sender(value, outbox, world, sender) },
    ).await
}
