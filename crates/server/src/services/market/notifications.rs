//! Private order notices for `marketProxy` (`eve/client/script/ui/services/marketsvc.py`).
//! Uses game corporation division authority; NPC orders have no private audience.
//! Outbox delivery follows commit and is ephemeral, not an exactly-once notification log.
use super::{order_keyval, CallError, Order};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::services::corpwallet;
use marshal::Value;

pub(super) async fn tell_owner<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, order: &Order, reason: &str) -> Result<(), CallError> {
    let recipients = if let Some(corp) = order.corporation_id {
        corpwallet::readable_members(ex, corp, order.wallet_key).await?
    } else { order.char_id.into_iter().collect() };
    let row = order_keyval(order);
    for character in recipients {
        outbox.notify_pilot(character, "OnOwnOrdersChanged", vec![Value::list([row.clone()]), Value::str(reason), Value::Bool(order.corporation_id.is_some())]);
    }
    Ok(())
}
