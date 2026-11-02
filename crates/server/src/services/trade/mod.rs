//! Direct player trades in station hangars, mirroring `pvptradesvc.py` and
//! `pvptradewnd.py`. Durable state is stored in `game.trades` and
//! `game.trade_items`; the in-memory index is a post-commit cache only.
//! Corporation items, fitted modules, charges and PLEX-vault transfers are not
//! supported. Idle trades are cancelled by the durable scheduler.
mod session;
mod settle;
mod trade_mgr;
mod timeout;

use std::{collections::HashMap, sync::{Mutex, OnceLock}};
use crate::{client::Client, dao::trades::Trade, db::outbox::{Effect, Outbox}, services::{Call, CallError, CallResult}, space::World};

static STATE: OnceLock<Mutex<HashMap<i64, Trade>>> = OnceLock::new();
fn state() -> &'static Mutex<HashMap<i64, Trade>> { STATE.get_or_init(|| Mutex::new(HashMap::new())) }

pub(crate) fn is_trade_container(id: i64) -> bool { state().lock().unwrap().contains_key(&id) }

pub(crate) enum TradeEffect { Put(Trade), Remove(i64), WakeScheduler }

pub(crate) fn dispatch<T>(value: T, outbox: Outbox<TradeEffect>, world: &World) -> Result<T, CallError> {
    for effect in outbox.into_entries() {
        match effect {
            Effect::Notification(notification) => notification.dispatch(world, None)?,
            Effect::Operation(TradeEffect::Put(trade)) => { state().lock().unwrap().insert(trade.id, trade); }
            Effect::Operation(TradeEffect::Remove(id)) => { state().lock().unwrap().remove(&id); }
            Effect::Operation(TradeEffect::WakeScheduler) => crate::scheduler::wake(),
            Effect::Event(event) => crate::lua::on_event(event),
        }
    }
    Ok(value)
}

pub(super) fn timeout_registration() -> crate::scheduler::Registration { timeout::registration() }

pub(super) async fn route(c: &mut Client, call: &Call<'_>) -> CallResult {
    trade_mgr::dispatch(c, call).await
}

pub(crate) async fn abort_for_char(ex: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut Outbox<TradeEffect>, char_id: i64) -> Result<(), CallError> {
    settle::abort_for_char(ex, sde, outbox, char_id).await
}

pub(crate) async fn on_leave(identity: &crate::services::identity::Identity, db: &crate::db::Db, world: &World, sde: &std::sync::Arc<crate::sde::Sde>) -> Result<(), CallError> {
    if identity.char_id == 0 { return Ok(()); }
    settle::leave(db, world, sde.clone(), identity.char_id).await
}

/// The caller holds the economy lock through the subsequent departure.
pub(crate) async fn on_leave_locked(db: &crate::db::Db, world: &World, sde: &std::sync::Arc<crate::sde::Sde>, char_id: i64) -> Result<(), CallError> {
    if char_id == 0 { return Ok(()); }
    settle::leave_locked(db, world, sde.clone(), char_id).await
}

pub(crate) async fn recover(db: &crate::db::Db, world: &World, sde: std::sync::Arc<crate::sde::Sde>) -> Result<(), CallError> {
    settle::recover(db, world, sde).await
}

#[cfg(test)] mod tests;
