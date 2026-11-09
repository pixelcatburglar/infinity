//! Non-diminishing injector credit read for the client cache.
//! eve/client/script/ui/services/nonDiminishingInjectionService.py:27-50 caches
//! the scalar and updates it from added, used and removed notifications.
//! Uses active game.items and game.item_attributes with SDE type defaults.
//! With no selected character the credit count is zero without a database read.
//! Native booster charge policy beyond persisted active uses is not modelled.
use super::{db_err, trading, Call, CallResult};
use crate::{client::{filetime_now, Client}, db::outbox::Outbox};
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if (call.service, call.method) != ("nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections") { return None; }
    if matches!(c.session.get("charid"), Value::None) { return Some(Ok(Value::Int(0))); }
    let char_id = c.session.int("charid").unwrap_or(0);
    let now = filetime_now();
    let _lock = crate::services::market::lock().await;
    let sde = c.sde.clone();
    Some(c.db.transaction(|tx, _: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            Ok(Value::Int(trading::available_credits(tx, &sde, char_id, now).await?))
        })
    }, |value, _: Outbox| async move { Ok(value) }).await)
}

#[cfg(test)] mod tests;
