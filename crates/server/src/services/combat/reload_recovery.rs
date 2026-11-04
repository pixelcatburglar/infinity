//! Return interrupted ammunition reload reservations during startup or ship repackaging.
//!
//! Mirrors reload handling in `eve/client/script/dogma/clientDogmaLocation.py`;
//! reads `game.pending_reloads` and `game.items`. Reload timers remain in memory and
//! are not resumed: surviving ships get reserved charges back in cargo. A missing
//! ship has lost its in-flight charges, so recovery deletes the reservation only.

use std::{collections::BTreeMap, sync::Arc};

use crate::consts::FLAG_CARGO;
use crate::dao::items::{pending_reloads, ship};
use crate::db::{Db, executor::Executor, outbox::Outbox};
use crate::messages;
use crate::sde::Sde;
use crate::services::{CallError, db_err, items::deposit, notify_error};

/// The caller must hold the hull lock; reload IDs must be ascending and belong to that ship.
pub(in crate::services) async fn return_pending<X: Send>(tx: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, reload_ids: &[i64], ship_exists: bool) -> Result<(), CallError> {
    let mut groups = BTreeMap::<(i64, i64, i32), i64>::new();
    for &reload_id in reload_ids {
        let Some(row) = pending_reloads::take(tx, reload_id).await.map_err(db_err)? else { continue };
        if ship_exists {
            let total = groups.entry((row.owner_id, row.ship_id, row.type_id)).or_default();
            *total = total.checked_add(i64::from(row.quantity)).ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
        }
    }
    for ((owner, ship, type_id), quantity) in groups {
        let quantity = i32::try_from(quantity).map_err(|_| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
        deposit(tx, sde, outbox, owner, ship, FLAG_CARGO, type_id, quantity, ship).await?;
    }
    Ok(())
}

pub(crate) async fn recover_pending_reloads(db: &Db, sde: &Arc<Sde>) -> Result<(), CallError> {
    let _lock = crate::services::market::lock().await;
    for row in pending_reloads::all(db).await.map_err(db_err)? {
        let (reload_id, ship_id) = (row.reload_id, row.ship_id);
        db.transaction(
            |tx, outbox: &mut Outbox| {
                let sde = Arc::clone(sde);
                Box::pin(async move {
                    let exists = ship::lock_reload_ship(tx, ship_id).await.map_err(db_err)?;
                    return_pending(tx, &sde, outbox, &[reload_id], exists).await
                })
            },
            |(), _outbox| async move { Ok(()) },
        ).await?;
    }
    Ok(())
}
