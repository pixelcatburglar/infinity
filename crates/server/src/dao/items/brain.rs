//! Current-body augmentation metadata on `game.items`; dormant clone implants
//! are accessed only through the clone custody DAO. Client consumers are the
//! skill and dogma services; callers own locks and lifecycle policy.
use crate::consts::{FLAG_BOOSTER, FLAG_IMPLANT};
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BrainItem {
    pub item_id: i64,
    pub type_id: i32,
    pub flag_id: i32,
    pub slot: Option<i32>,
    pub activated_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub effect_ids: Option<Vec<i32>>,
}
impl BrainItem {
    pub(super) fn from_row(row: &Row) -> Self {
        Self { item_id: row.get("item_id"), type_id: row.get("type_id"), flag_id: row.get("flag_id"),
            slot: row.get("brain_slot"), activated_at: row.get("brain_activated_at"),
            expires_at: row.get("brain_expires_at"), effect_ids: row.get("brain_effect_ids") }
    }
}

/// All installed rows, including expired and legacy rows. Callers serialize mutations by character.
pub(crate) async fn list(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Vec<BrainItem>, Error> {
    Ok(ex.query("SELECT item_id, type_id, flag_id, brain_slot, brain_activated_at, brain_expires_at, brain_effect_ids FROM game.items WHERE owner_id=$1 AND location_id=$1 AND flag_id IN ($2,$3) AND jump_clone_id IS NULL ORDER BY item_id", &[&char_id, &FLAG_BOOSTER, &FLAG_IMPLANT]).await?.iter().map(BrainItem::from_row).collect())
}

pub(crate) async fn get(ex: &(impl Executor + ?Sized), char_id: i64, item_id: i64) -> Result<Option<BrainItem>, Error> {
    Ok(ex.query_opt("SELECT item_id, type_id, flag_id, brain_slot, brain_activated_at, brain_expires_at, brain_effect_ids FROM game.items WHERE owner_id=$1 AND location_id=$1 AND item_id=$2 AND flag_id IN ($3,$4) AND jump_clone_id IS NULL", &[&char_id, &item_id, &FLAG_BOOSTER, &FLAG_IMPLANT]).await?.as_ref().map(BrainItem::from_row))
}

/// Includes overdue deadlines; legacy rows without expiry have no deadline.
pub(crate) async fn earliest_expiry(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<i64>, Error> {
    let row = ex.query_one("SELECT min(brain_expires_at) AS expiry FROM game.items WHERE owner_id=$1 AND location_id=$1 AND flag_id=$2", &[&char_id, &FLAG_BOOSTER]).await?;
    Ok(row.get("expiry"))
}

pub(crate) async fn timed_characters(ex: &(impl Executor + ?Sized)) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT DISTINCT owner_id FROM game.items WHERE owner_id=location_id AND flag_id=$1 AND brain_expires_at IS NOT NULL ORDER BY owner_id", &[&FLAG_BOOSTER]).await?.iter().map(|row| row.get("owner_id")).collect())
}

/// Move an observed singleton into its brain slot. The character lock must be held.
pub(crate) async fn install(ex: &(impl Executor + ?Sized), item_id: i64, char_id: i64, old_location: i64, old_flag: i32, flag: i32, slot: i32, activated_at: Option<i64>, expires_at: Option<i64>, effect_ids: Option<&[i32]>) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.items SET location_id=$2, flag_id=$5, brain_slot=$6, brain_activated_at=$7, brain_expires_at=$8, brain_effect_ids=$9 WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND flag_id=$4 AND quantity=1 AND singleton AND brain_slot IS NULL AND jump_clone_id IS NULL", &[&item_id, &char_id, &old_location, &old_flag, &flag, &slot, &activated_at, &expires_at, &effect_ids]).await? == 1)
}

/// Update metadata only if the complete observed state remains unchanged.
pub(crate) async fn update_metadata(ex: &(impl Executor + ?Sized), char_id: i64, old: &BrainItem, slot: Option<i32>, activated_at: Option<i64>, expires_at: Option<i64>, effect_ids: Option<&[i32]>) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.items SET brain_slot=$9, brain_activated_at=$10, brain_expires_at=$11, brain_effect_ids=$12 WHERE owner_id=$1 AND location_id=$1 AND item_id=$2 AND type_id=$3 AND flag_id=$4 AND brain_slot IS NOT DISTINCT FROM $5 AND brain_activated_at IS NOT DISTINCT FROM $6 AND brain_expires_at IS NOT DISTINCT FROM $7 AND brain_effect_ids IS NOT DISTINCT FROM $8 AND jump_clone_id IS NULL", &[&char_id, &old.item_id, &old.type_id, &old.flag_id, &old.slot, &old.activated_at, &old.expires_at, &old.effect_ids, &slot, &activated_at, &expires_at, &effect_ids]).await? == 1)
}

/// Delete the observed installed item; its item attributes cascade.
pub(crate) async fn remove(ex: &(impl Executor + ?Sized), char_id: i64, old: &BrainItem) -> Result<bool, Error> {
    Ok(ex.execute("DELETE FROM game.items WHERE owner_id=$1 AND location_id=$1 AND item_id=$2 AND type_id=$3 AND flag_id=$4 AND brain_slot IS NOT DISTINCT FROM $5 AND brain_activated_at IS NOT DISTINCT FROM $6 AND brain_expires_at IS NOT DISTINCT FROM $7 AND brain_effect_ids IS NOT DISTINCT FROM $8 AND jump_clone_id IS NULL", &[&char_id, &old.item_id, &old.type_id, &old.flag_id, &old.slot, &old.activated_at, &old.expires_at, &old.effect_ids]).await? == 1)
}
