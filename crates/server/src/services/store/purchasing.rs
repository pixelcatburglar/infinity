//! Atomic paid store purchases for `vgs/client/vgsClient.py`'s `buy_offer`.
//! Persists PLEX in `game.accounts`, receipts in `game.store_purchases`, tokens in
//! `game.redeem_tokens`, and resolves gifts through `game.characters`. External
//! checkout and daily free gifts are not handled here.
use super::{db_err, TOKEN_LABEL};
use crate::{dao::store::purchases, db::outbox::Outbox, live_sessions, services::{CallError, plex_vault::{self, VaultAudience, VaultChange, VaultContext, VaultKind}}};
use tokio_postgres::Transaction;


#[derive(Clone, Copy)]
pub(super) struct Purchase {
    offer_id: i32,
    type_id: i32,
    count: i64,
    cost: i64,
    delivery: i64,
}

impl Purchase {
    pub(super) fn new(offer_id: i64, type_id: i32, unit_quantity: i64, unit_price: i64, count: i64) -> Option<Self> {
        let offer_id = i32::try_from(offer_id).ok()?;
        let delivery = unit_quantity.checked_mul(count)?;
        let cost = unit_price.checked_mul(count)?;
        if offer_id <= 0 || type_id <= 0 || unit_quantity <= 0 || unit_price < 0 || count <= 0 {
            return None;
        }
        Some(Self { offer_id, type_id, count, cost, delivery })
    }
}

pub(super) async fn purchase(
    tx: &Transaction<'_>,
    outbox: &mut Outbox,
    payer: i64,
    gift_to: Option<i64>,
    purchase: &Purchase,
    now: i64,
    audience: VaultAudience,
) -> Result<bool, CallError> {
    if gift_to.is_some_and(|character_id| character_id <= 0) {
        return Ok(false);
    }
    let recipient = match gift_to {
        Some(character_id) => match crate::dao::characters::persistence::user_for_character(tx, character_id).await.map_err(db_err)? {
            Some(user_id) => user_id,
            None => return Ok(false),
        },
        None => payer,
    };
    let Some(_) = plex_vault::change_in(tx, outbox, payer, None, VaultChange::Add(-i128::from(purchase.cost)), VaultContext { kind: VaultKind::StorePurchase, item_id: None, reference: None }, audience).await? else {
        return Ok(false);
    };
    purchases::purchase(tx, payer, purchase.offer_id, purchase.count, purchase.cost).await.map_err(db_err)?;
    purchases::grant(tx, recipient, purchase.type_id, purchase.delivery, TOKEN_LABEL, now).await.map_err(db_err)?;

    if recipient == payer {
        outbox.notify_client("OnRedeemingQueueUpdated", vec![]);
    } else if let Some(sender) = live_sessions::sender(recipient) {
        outbox.notify_sender(sender, "OnRedeemingQueueUpdated", vec![]);
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
