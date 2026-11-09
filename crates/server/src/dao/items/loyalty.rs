//! LP store requirement stacks in `game.items`, for the client's station hangar.
//! Type metadata comes from the loaded SDE; settlement and quantity mutations are caller concerns.
//! Singleton items and stacks outside the owner's station hangar are not eligible.

use infinity_core::consts::FLAG_HANGAR;
use crate::{db::{self, executor::Executor}, sde::Sde};
use super::loading::Item;

/// Return eligible stacks in item ID order for guarded changes via `quantities::apply`.
pub(crate) async fn requirement_stacks(
    ex: &(impl Executor + ?Sized), sde: &Sde, owner_id: i64, location_id: i64, type_ids: &[i32],
) -> Result<Vec<Item>, db::Error> {
    let rows = ex.query(
        "SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info FROM game.items WHERE owner_id = $1 AND location_id = $2 AND flag_id = $3 AND NOT singleton AND type_id = ANY($4) ORDER BY item_id",
        &[&owner_id, &location_id, &FLAG_HANGAR, &type_ids],
    ).await?;
    Ok(rows.iter().map(|row| {
        let type_id: i32 = row.get("type_id");
        let data = sde.types.get(type_id);
        Item {
            item_id: row.get("item_id"), type_id, owner_id: row.get("owner_id"),
            location_id: row.get("location_id"), flag_id: row.get("flag_id"),
            quantity: row.get("quantity"), singleton: row.get("singleton"),
            group_id: i64::from(data.and_then(|t| t.group_id).unwrap_or(0)),
            category_id: i64::from(data.and_then(|t| t.category_id).unwrap_or(0)),
            custom_info: row.get("custom_info"),
        }
    }).collect())
}
