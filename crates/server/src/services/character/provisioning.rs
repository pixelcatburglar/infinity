//! Persistent rookie-ship provisioning before a pilot session exists.
//! Character selection calls `charUnboundMgr.SelectCharacterID` in
//! `eve/client/script/ui/login/charSelection/characterSelection.py:789`; entry after
//! creation is in `eve/client/script/ui/services/ccSvc.py:186`. Race ships come
//! from the SDE, while character and item state comes from the game database.
//! The embedded server-authored catalogue supplies civilian fits. Existing ships
//! are not retroactively fitted, and the fits have no charges.

use infinity_core::consts::{FALLBACK_ROOKIE_SHIP, FLAG_HANGAR};
use crate::dao::{characters, items};
use crate::db::{outbox::Outbox, Db};
use crate::sde::Sde;
use crate::services::CallError;

/// Give the character a rookie ship in their station hangar if they have none.
pub(crate) async fn ensure_ship(db: &Db, sde: &Sde, char_id: i64, race_id: i32, station_id: i64) -> Result<i64, CallError> {
    let ship_type = sde.characters.race(race_id as i64).and_then(|race| race.ship_type_id).unwrap_or(FALLBACK_ROOKIE_SHIP);
    let rookie_fits = sde.rookie_fits.clone();
    let ship_id = db.transaction(
        |tx, _: &mut Outbox| {
            let rookie_fits = rookie_fits.clone();
            Box::pin(async move {
                characters::selection::lock_character(tx, char_id).await?;
                if let Some(ship_id) = characters::selection::ship_id(tx, char_id).await? {
                    return Ok::<i64, CallError>(ship_id);
                }
                let fit = rookie_fits.get(ship_type)
                    .ok_or_else(|| CallError::Internal(anyhow::anyhow!("no rookie fit for ship type {ship_type}")))?;
                let ship_id = items::creation::rookie_ship(tx, fit.ship, char_id, station_id, FLAG_HANGAR).await?;
                items::creation::fitted_items(tx, ship_id, char_id, &fit.items).await?;
                characters::selection::set_ship(tx, char_id, ship_id).await?;
                Ok(ship_id)
            })
        },
        |value, _| async move { Ok(value) },
    ).await?;
    Ok(ship_id)
}

#[cfg(test)]
mod tests;
