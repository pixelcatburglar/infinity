//! Escrow custody moves and withdrawal re-keying of `game.items` rows for the
//! repairable-ship recovery lifecycle, plus the attachment migration that
//! preserves non-HP overrides, dynamic provenance, service-life damage and
//! hull metadata across the new inventory identity.
//! The escrow shape is `owner = InterBus`, `location = recovery group`; rows
//! carry no foreign key back into recovery tables. Writes verify the caller's
//! `LockedItems` token; transactions, refusals, notifications and World are
//! caller concerns.

use std::collections::BTreeMap;
use infinity_core::consts::{FLAG_HANGAR, NPC_CORP_INTERBUS};
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_SHIELD_CHARGE};
use crate::db::{self, executor::Executor};
use crate::dao::items::parent_locks::LockedItems;
use crate::dao::repairable_ship::{ItemId, RecoveryGroupId, Selection, SourceShape};

/// HP condition attributes dropped on withdrawal: the restored item's type and
/// dynamic bases re-derive full shield, armor and hull.
const CONDITION_ATTRIBUTES: [i32; 3] = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];

/// The fenced original rows of pending losses: every durable column reconcile
/// compares, in one statement.
pub(crate) async fn capture_many(ex: &(impl Executor + ?Sized), ids: &[ItemId]) -> Result<Vec<SourceShape>, db::Error> {
    let raw: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    ex.query(
        "SELECT item_id, owner_id, type_id, location_id, flag_id, quantity, singleton, x, y, z, brain_slot, jump_clone_id, \
         (brain_activated_at IS NULL AND brain_expires_at IS NULL AND brain_effect_ids IS NULL) AS brain_clear \
         FROM game.items WHERE item_id = ANY($1)",
        &[&raw],
    ).await?.iter().map(|row| {
        if !row.get::<_, bool>("brain_clear") {
            return Err(db::Error::Progress(anyhow::anyhow!("item contains brain state")));
        }
        SourceShape::from_row(row)
    }).collect()
}

/// `online` module rows among the requested items, for workshop refusal.
pub(crate) async fn online(ex: &(impl Executor + ?Sized), ids: &[ItemId]) -> Result<Vec<ItemId>, db::Error> {
    let raw: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    ex.query("SELECT item_id FROM game.items WHERE item_id = ANY($1) AND online", &[&raw]).await?
        .iter()
        .map(|row| ItemId::new(row.get("item_id"))
            .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("online row is not a physical item"))))
        .collect()
}

/// Dynamic-item provenance source types for the requested items; absent rows
/// mean the item is not dynamic and prices by its own type.
pub(crate) async fn dynamic_sources(ex: &(impl Executor + ?Sized), ids: &[ItemId]) -> Result<BTreeMap<ItemId, i32>, db::Error> {
    let raw: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    ex.query("SELECT item_id, source_type_id FROM game.dynamic_items WHERE item_id = ANY($1)", &[&raw]).await?
        .iter()
        .map(|row| {
            let id: i64 = row.get("item_id");
            ItemId::new(id).map(|id| (id, row.get("source_type_id")))
                .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("dynamic provenance row {id} is not a physical item")))
        })
        .collect()
}

/// Move the fenced original rows into escrow: `owner = InterBus`,
/// `location = group`. Every supplied shape must still match the live row
/// column-for-column; a write lock on every row is required.
pub(crate) async fn move_to_custody(
    ex: &(impl Executor + ?Sized), locks: &LockedItems<'_>, group: RecoveryGroupId, rows: &[SourceShape],
) -> Result<bool, db::Error> {
    for shape in rows {
        if !locks.is_write_locked(shape.item.get()) { return Ok(false); }
        let affected = ex.execute(
            "UPDATE game.items SET owner_id = $2, location_id = $3 \
             WHERE item_id = $1 AND owner_id = $4 AND type_id = $5 AND location_id = $6 AND flag_id = $7 \
             AND quantity = $8 AND singleton = $9 \
             AND x IS NOT DISTINCT FROM $10 AND y IS NOT DISTINCT FROM $11 AND z IS NOT DISTINCT FROM $12 \
             AND brain_slot IS NULL AND brain_activated_at IS NULL AND brain_expires_at IS NULL \
             AND brain_effect_ids IS NULL AND jump_clone_id IS NULL",
            &[&shape.item.get(), &NPC_CORP_INTERBUS, &group.get(),
                &shape.owner, &shape.type_id, &shape.location, &shape.flag, &shape.quantity, &shape.singleton,
                &shape.x, &shape.y, &shape.z],
        ).await?;
        if affected != 1 { return Ok(false); }
    }
    Ok(true)
}

/// The live escrow row's preserved columns; `custom_info` and `name` are not
/// part of the recovery record and are read back for the replacement row.
struct EscrowRow {
    custom_info: Option<String>,
    name: Option<String>,
}

/// Re-key one escrowed member: a new `game.items` row for the beneficiary at
/// the group's frozen NPC destination station, all preserved attachments
/// migrated, then (caller-side) the member mapping is saved and the old row
/// deleted in the same transaction. `None` means the escrow row is missing or
/// no longer matches the member's recorded shape, or the selection is not
/// exactly one member.
pub(crate) async fn restore_new(
    ex: &(impl Executor + ?Sized), locks: &LockedItems<'_>, selection: &Selection, now: i64,
) -> Result<Option<ItemId>, db::Error> {
    let [member] = selection.items.as_slice() else { return Ok(None) };
    if !locks.is_write_locked(member.item.get()) { return Ok(None); }
    let row = ex.query_opt(
        "SELECT custom_info, name FROM game.items \
         WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND type_id = $4 AND flag_id = $5 \
         AND quantity = $6 AND singleton = $7 AND brain_slot IS NULL AND brain_activated_at IS NULL \
         AND brain_expires_at IS NULL AND brain_effect_ids IS NULL AND jump_clone_id IS NULL",
        &[&member.item.get(), &NPC_CORP_INTERBUS, &selection.group.id.get(), &member.type_id, &member.flag,
            &member.quantity, &member.singleton],
    ).await?;
    let Some(row) = row else { return Ok(None) };
    let row = EscrowRow { custom_info: row.get("custom_info"), name: row.get("name") };
    let new_id: i64 = ex.query_one(
        "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info, name) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING item_id",
        &[&member.type_id, &selection.group.beneficiary, &selection.group.destination_station, &FLAG_HANGAR,
            &member.quantity, &member.singleton, &row.custom_info, &row.name],
    ).await?.get("item_id");
    let new = ItemId::new(new_id)
        .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("restored item id {new_id} outside the physical band")))?;
    let old = member.item.get();
    // Non-HP overrides keep every persisted column; HP condition resets.
    ex.execute(
        "INSERT INTO game.item_attributes (item_id, attribute_id, value, updated_at, cargo_unlimited) \
         SELECT $1, attribute_id, value, updated_at, cargo_unlimited FROM game.item_attributes \
         WHERE item_id = $2 AND attribute_id <> ALL($3)",
        &[&new.get(), &old, &CONDITION_ATTRIBUTES.as_slice()],
    ).await?;
    // Provenance first: modifier rows reference the dynamic parent.
    ex.execute(
        "INSERT INTO game.dynamic_items (item_id, source_item_id, mutator_item_id, source_type_id, mutator_type_id, result_type_id, creator_id, created_at) \
         SELECT $1, source_item_id, mutator_item_id, source_type_id, mutator_type_id, result_type_id, creator_id, created_at \
         FROM game.dynamic_items WHERE item_id = $2",
        &[&new.get(), &old],
    ).await?;
    ex.execute(
        "INSERT INTO game.dynamic_item_modifiers (item_id, attribute_id, factor) \
         SELECT $1, attribute_id, factor FROM game.dynamic_item_modifiers WHERE item_id = $2",
        &[&new.get(), &old],
    ).await?;
    ex.execute(
        "INSERT INTO game.item_service_life (item_id, damage) SELECT $1, damage FROM game.item_service_life WHERE item_id = $2",
        &[&new.get(), &old],
    ).await?;
    // Sharing switches follow the new hull owner.
    ex.execute(
        "INSERT INTO game.ship_configuration (ship_item_id, owner_id, fleet_hangar_fleet, fleet_hangar_corp, maintenance_fleet, maintenance_corp) \
         SELECT $1, $3, fleet_hangar_fleet, fleet_hangar_corp, maintenance_fleet, maintenance_corp \
         FROM game.ship_configuration WHERE ship_item_id = $2 AND owner_id = $3",
        &[&new.get(), &old, &selection.group.beneficiary],
    ).await?;
    // The SKIN is restored only under the beneficiary's own live license.
    ex.execute(
        "INSERT INTO game.ship_skin_applications (ship_item_id, character_id, skin_id) \
         SELECT $1, l.character_id, a.skin_id FROM game.ship_skin_applications a \
         JOIN game.ship_skin_licenses l ON l.character_id = $3 AND l.skin_id = a.skin_id \
             AND (l.expires_at IS NULL OR l.expires_at >= $4) \
         WHERE a.ship_item_id = $2 AND a.character_id = $3",
        &[&new.get(), &old, &selection.group.beneficiary, &now],
    ).await?;
    ex.execute(
        "INSERT INTO game.ship_dirt (ship_item_id, dirt_timestamp) SELECT $1, dirt_timestamp FROM game.ship_dirt WHERE ship_item_id = $2",
        &[&new.get(), &old],
    ).await?;
    ex.execute(
        "INSERT INTO game.ship_kill_counts (ship_item_id, player_kills) SELECT $1, player_kills FROM game.ship_kill_counts WHERE ship_item_id = $2",
        &[&new.get(), &old],
    ).await?;
    Ok(Some(new))
}

/// Delete the escrowed physical rows of a selection whose recorded shape still
/// matches; a write lock on every member's row is required.
pub(crate) async fn delete_custody(
    ex: &(impl Executor + ?Sized), locks: &LockedItems<'_>, selection: &Selection,
) -> Result<bool, db::Error> {
    for member in &selection.items {
        if !locks.is_write_locked(member.item.get()) { return Ok(false); }
        let affected = ex.execute(
            "DELETE FROM game.items \
             WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND type_id = $4 AND flag_id = $5 \
             AND quantity = $6 AND singleton = $7 AND brain_slot IS NULL AND brain_activated_at IS NULL \
             AND brain_expires_at IS NULL AND brain_effect_ids IS NULL AND jump_clone_id IS NULL",
            &[&member.item.get(), &NPC_CORP_INTERBUS, &selection.group.id.get(), &member.type_id, &member.flag,
                &member.quantity, &member.singleton],
        ).await?;
        if affected != 1 { return Ok(false); }
    }
    Ok(true)
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
