//! Station clone lifecycle for clonejumpsvc.py, backed by game.jump_clones
//! and real dormant game.items. Body transitions and ship clones are separate.
//! Callers hold the market guard and transaction through post-commit dispatch.

use std::sync::Arc;
use marshal::Value;
use infinity_core::messages;
use crate::{dao::{characters::clones as clone_dao, items::clones as item_dao}, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::{db_err, notify_error, CallError, identity::Identity, wallets}};
use super::{policy, CloneEffect, MAX_CLONE_NAME_LENGTH};

fn missing() -> CallError { notify_error(messages::JUMP_CLONE_NOT_FOUND) }

fn invalidate<E>(outbox: &mut Outbox<E>, owner: i64, location: i64) {
    outbox.notify_pilot(owner, "OnJumpCloneCacheInvalidated", vec![]);
    outbox.notify_pilot(owner, "OnStationJumpCloneCacheInvalidated", vec![Value::Int(location), Value::Int(owner)]);
}

/// Caller owns the transaction and has locked the authoritative character.
pub(super) async fn create_empty_clone_tx(ex: &(impl Executor + ?Sized), owner: i64, location: i64) -> Result<clone_dao::CloneRow, CallError> {
    clone_dao::create(ex, owner, location).await.map_err(db_err)
}

/// Caller has authorized the owner and locked the character, clone and items.
/// Deletes actual dormant implants and their cascading attributes, never the active brain.
pub(super) async fn destroy_clone_and_implants_tx<E>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, owner: i64, clone: &clone_dao::CloneRow) -> Result<(), CallError> {
    if clone.owner_id != owner { return Err(missing()); }
    let implants = item_dao::list(ex, owner, clone.clone_id).await.map_err(db_err)?;
    if item_dao::destroy(ex, owner, clone.clone_id).await.map_err(db_err)? != implants.len() as u64 { return Err(missing()); }
    if clone_dao::destroy(ex, clone.clone_id, owner).await.map_err(db_err)? != 1 { return Err(missing()); }
    invalidate(outbox, owner, clone.location_id);
    Ok(())
}

/// Caller holds the market guard and transaction; policy locks the owner and clones.
pub(super) async fn install_tx(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<CloneEffect>, sde: &Arc<Sde>, id: &Identity, now: i64) -> Result<(), CallError> {
    policy::ensure_install_tx(ex, sde, id, now).await?;
    let station = id.station_id.ok_or_else(missing)?;
    create_empty_clone_tx(ex, id.char_id, station).await?;
    wallets::debit(ex, outbox, id.char_id, crate::isk::IskAmount::from_isk(policy::service_fee())?).await?;
    invalidate(outbox, id.char_id, station);
    Ok(())
}

/// Caller holds the market guard and transaction.
pub(super) async fn destroy_tx(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<CloneEffect>, id: &Identity, clone_id: i64) -> Result<(), CallError> {
    clone_dao::lock_character(ex, id.char_id).await.map_err(db_err)?.ok_or_else(missing)?;
    let clones = clone_dao::lock_clones(ex, id.char_id).await.map_err(db_err)?;
    let clone = clones.iter().find(|clone| clone.clone_id == clone_id).ok_or_else(missing)?;
    item_dao::lock(ex, id.char_id).await.map_err(db_err)?;
    destroy_clone_and_implants_tx(ex, outbox, id.char_id, clone).await
}

/// Caller holds the market guard and transaction.
pub(super) async fn rename_tx(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<CloneEffect>, id: &Identity, clone_id: i64, name: &str) -> Result<(), CallError> {
    if name.chars().count() > MAX_CLONE_NAME_LENGTH || name.contains('\0') { return Err(notify_error(messages::clones::INVALID_CLONE_NAME)); }
    clone_dao::lock_character(ex, id.char_id).await.map_err(db_err)?.ok_or_else(missing)?;
    let clones = clone_dao::lock_clones(ex, id.char_id).await.map_err(db_err)?;
    let clone = clones.iter().find(|clone| clone.clone_id == clone_id).ok_or_else(missing)?;
    if clone_dao::rename(ex, clone_id, id.char_id, name).await.map_err(db_err)? != 1 { return Err(missing()); }
    invalidate(outbox, id.char_id, clone.location_id);
    Ok(())
}

/// Caller has checked the real GM or QA role and holds the transaction.
pub(super) async fn reset_tx(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<CloneEffect>, id: &Identity) -> Result<(), CallError> {
    let character = clone_dao::lock_character(ex, id.char_id).await.map_err(db_err)?.ok_or_else(missing)?;
    if clone_dao::reset_last_jump(ex, id.char_id).await.map_err(db_err)? != 1 { return Err(missing()); }
    outbox.notify_pilot(id.char_id, "OnJumpCloneCacheInvalidated", vec![]);
    if let Some(location) = character.station_id {
        outbox.notify_pilot(id.char_id, "OnStationJumpCloneCacheInvalidated", vec![Value::Int(location), Value::Int(id.char_id)]);
    }
    Ok(())
}

#[cfg(test)] mod tests;
