//! Transactional destruction of persistent unpiloted ships and drones.
//! These losses consume ineligible contents and reservations and clear combat
//! attachments and insurance; eligible personal items enter recovery custody.
//! They create no player killmail, capsule, wreck or reward. Live health is
//! owned by the system actor.
use super::*;
use crate::space::combat::FirstLethalSnapshot;

pub(super) async fn destroy_in<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut crate::db::outbox::Outbox<X>, sde: &Sde,
    system: i64, snapshot: &FirstLethalSnapshot, pending: &crate::services::ship::loss::facts::PlayerLossContext,
    loaded: &[crate::dao::items::charges::LoadedWrite],
) -> Result<bool, CallError> {
    let ship = snapshot.item_id;
    let Some(kind) = snapshot.kind else { return Ok(false) };
    let Some(locks) = crate::services::ship::loss::lock_loss_items(ex, sde, ship, None).await? else { return Ok(false) };
    let claimed = if snapshot.is_drone {
        crate::dao::items::drones::claim_lost(ex, ship, kind, snapshot.owner_id, system).await.map_err(db_err)?
    } else {
        crate::dao::items::space_ships::claim_unpiloted(ex, ship, kind, snapshot.owner_id, system).await.map_err(db_err)?
    };
    if !claimed { return Ok(false); }
    // The victim's own loaded absolute commits before the freeze and item
    // deletes; an NPC without real modules settles an empty batch here.
    super::loaded::settle_in(ex, loaded).await?;
    let kind_source = if snapshot.is_drone { crate::dao::repairable_ship::SourceKind::Drone }
        else { crate::dao::repairable_ship::SourceKind::UnpilotedShip };
    let source = crate::services::ship::loss::facts::freeze_source(
        ex, pending, kind_source, ship, snapshot.owner_id, kind, system, snapshot.lethal_at, snapshot.position).await?;
    let character = crate::dao::corporations::registry::active_character(ex, snapshot.owner_id).await.map_err(db_err)?;
    let capture = if character {
        Some(crate::services::repairable_ship::recovery::capture_loss_in(ex, outbox, sde, &locks, &source, pending.recovery_seed).await?)
    } else { None };
    crate::dao::insurance::take_payout(ex, ship).await.map_err(db_err)?;
    crate::dao::items::ship::delete_lost(ex, ship, capture.as_ref().map_or(&[], |capture| capture.retained.as_slice())).await.map_err(db_err)?;
    if let Some(capture) = &capture {
        crate::services::repairable_ship::recovery::finish_custody_in(ex, outbox, &locks, capture).await?;
    }
    if !snapshot.is_drone {
        super::super::map::events::unpiloted_lost(ex, ship, system, snapshot.lethal_at).await?;
    }
    Ok(true)
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;
