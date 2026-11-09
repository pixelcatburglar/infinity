//! Asset source, delivery and refund policy for `marketProxy`
//! (`eve/client/script/ui/services/marketsvc.py`). Uses game item and office DAOs and SDE
//! types. Corporate buys go to Deliveries at the fill station without an office; refunds
//! return to the original office/flag while it remains corporate custody, otherwise to
//! same-station Deliveries. Structures and shared ship bays are not market destinations
//! or sources, and asset ownership is not implicitly transferred between owners.
use super::{db_err, notify_error, CallError, Order, TradeParty};
use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, CORP_HANGAR_DIVISIONS, FLAG_HANGAR};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::db::{executor::Executor, outbox::Outbox};
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{inventory::{access::may_take_corp_hangar, destination::Destination, notifications::corp_item_viewers}, items, offices, user_error};
use crate::services::plex_vault::{self, VaultAudience, VaultChange, VaultContext, VaultKind};
use marshal::Value;
use std::sync::Arc;

pub(super) async fn source(ex: &(impl Executor + ?Sized), sde: &Sde, party: TradeParty, item_id: i64, station: i64, type_id: i64, quantity: i64, office: Option<i64>) -> Result<Item, CallError> {
    let refuse = || notify_error(messages::MARKET_SELL_ITEM_MUST_BE_IN_STATION_HANGAR);
    let item = items::load_items(ex, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop().ok_or_else(refuse)?;
    if item.owner_id != party.financial.id || i64::from(item.type_id) != type_id || quantity <= 0 || quantity > item.quantity { return Err(refuse()); }
    if party.financial.id == party.issuer {
        if office.is_some() || item.location_id != station || item.flag_id != FLAG_HANGAR { return Err(refuse()); }
    } else {
        if office != Some(item.location_id) || !(FLAG_CORP_SAG1..FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS).contains(&item.flag_id)
            || offices::office_owner(ex, item.location_id).await? != Some(party.financial.id)
            || crate::dao::offices::station(ex, item.location_id).await.map_err(db_err)? != Some(station)
            || !may_take_corp_hangar(ex, party.issuer, party.financial.id, station, item.flag_id).await? { return Err(refuse()); }
    }
    if crate::dao::industry::has_active_blueprint_job(ex, &[item.item_id]).await.map_err(db_err)? { return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE)); }
    if item.singleton { return Err(user_error(messages::MSG_REPACKAGE_BEFORE_SELLING, vec![("item".into(), Value::Int(type_id))])); }
    Ok(item)
}

async fn receipt<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, party: TradeParty, item: &Item) -> Result<(), CallError> {
    let recipients = if party.financial.id == party.issuer { vec![party.issuer] } else { corp_item_viewers(ex, item).await? };
    for character in recipients {
        outbox.notify_pilot(character, "OnMarketItemReceived", vec![Value::Int(item.item_id), Value::Int(i64::from(item.type_id)), Value::Int(i64::from(item.flag_id)), Value::Int(item.location_id)]);
    }
    Ok(())
}

async fn deposit<X: Send>(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, party: TradeParty, location: i64, flag: i32, type_id: i32, quantity: i64) -> Result<(), CallError> {
    let deposited = if party.financial.id == party.issuer {
        items::deliver(ex, sde, outbox, party.issuer, location, type_id, quantity).await?
    } else {
        items::deposit(ex, sde, outbox, &Destination::owned(party.financial.id, location, flag), type_id, quantity, 0).await?
    };
    for row in deposited.rows() { receipt(ex, outbox, party, &row.item).await?; }
    Ok(())
}

pub(super) async fn deliver<X: Send>(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, party: TradeParty, station: i64, type_id: i32, quantity: i64, vault_user_id: Option<i64>, order_id: i64, fill_id: i64) -> Result<(), CallError> {
    if type_id == infinity_core::consts::TYPE_PLEX && party.financial.id == party.issuer {
        let user_id = vault_user_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing market vault account")))?;
        let context = VaultContext { kind: VaultKind::MarketFill { order_id, trade_id: fill_id }, item_id: None, reference: None };
        plex_vault::change_in(ex, outbox, user_id, Some(party.issuer), VaultChange::Add(i128::from(quantity)), context, VaultAudience::Account).await?
            .ok_or_else(|| notify_error(infinity_core::messages::MARKET_ORDER_CLOSED))?;
        return Ok(());
    }
    let flag = if party.financial.id == party.issuer { FLAG_HANGAR } else { FLAG_CORP_DELIVERIES };
    deposit(ex, sde, outbox, party, station, flag, type_id, quantity).await
}

pub(super) async fn refund<X: Send>(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox<X>, order: &Order, party: TradeParty) -> Result<(), CallError> {
    if order.type_id == infinity_core::consts::TYPE_PLEX && order.corporation_id.is_none() {
        let user_id = order.vault_user_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing market vault account")))?;
        let context = VaultContext { kind: VaultKind::MarketRefund { order_id: order.id }, item_id: None, reference: None };
        plex_vault::change_in(ex, outbox, user_id, Some(party.issuer), VaultChange::Add(i128::from(order.vol_remaining)), context, VaultAudience::Account).await?
            .ok_or_else(|| notify_error(infinity_core::messages::MARKET_ORDER_CLOSED))?;
        return Ok(());
    }
    let (location, flag) = match (order.corporation_id, order.source_location_id, order.source_flag_id) {
        (Some(corp), Some(location), Some(flag)) if offices::office_owner(ex, location).await? == Some(corp) => (location, flag),
        (Some(_), _, _) => (order.at.id, FLAG_CORP_DELIVERIES),
        _ => (order.at.id, FLAG_HANGAR),
    };
    deposit(ex, sde, outbox, party, location, flag, order.type_id, order.vol_remaining).await
}
