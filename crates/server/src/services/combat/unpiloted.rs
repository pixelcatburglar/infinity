//! Transactional destruction of persistent unpiloted ships and drones.
//! These losses use game.items and insurance; they create no player killmail,
//! capsule, wreck or reward. Live health is owned by the system actor.
use super::*;
use crate::space::combat::FirstLethalSnapshot;

pub(super) async fn destroy_in(
    ex: &(impl Executor + ?Sized), sde: &Sde, system: i64, snapshot: &FirstLethalSnapshot,
) -> Result<bool, CallError> {
    let ship = snapshot.item_id;
    if snapshot.is_drone {
        return crate::dao::items::drones::destroy(ex, ship, snapshot.owner_id, system).await.map_err(db_err);
    }
    let Some(kind) = snapshot.kind else { return Ok(false) };
    let Some(_locks) = crate::services::ship::loss::lock_loss_items(ex, sde, ship).await? else { return Ok(false) };
    if !crate::dao::items::space_ships::claim_unpiloted(ex, ship, kind, snapshot.owner_id, system).await.map_err(db_err)? {
        return Ok(false);
    }
    for reload in crate::dao::items::pending_reloads::for_ship(ex, ship).await.map_err(db_err)? {
        crate::dao::items::pending_reloads::take(ex, reload.reload_id).await.map_err(db_err)?;
    }
    crate::dao::insurance::take_payout(ex, ship).await.map_err(db_err)?;
    crate::dao::items::ship::delete_lost(ex, ship).await.map_err(db_err)?;
    super::super::map::events::unpiloted_lost(ex, ship, system, snapshot.lethal_at).await?;
    Ok(true)
}

#[cfg(test)] mod tests;
