//! Persistence for ship assembly, guarded repackaging, saved fittings and blueprints.
//! Client services own policy, transactions and notification ordering.
use crate::db::{executor::Executor, Error};
use crate::dao::items::loading::Item;
use crate::consts::{BLUEPRINT_INFINITE_RUNS, INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY, FLAG_HANGAR};
use std::collections::BTreeMap;
use tokio_postgres::Row;

use super::inventory::{ItemId, Matched};
struct FittingId { fitting_id: i64 }
impl FittingId { fn from_row(r: &Row) -> Self { Self { fitting_id: r.get("fitting_id") } } }
pub(crate) struct Fitting { pub fitting_id: i64, pub owner_id: i64, pub name: String, pub description: String, pub ship_type_id: i32, pub fit_types: Vec<i32>, pub fit_flags: Vec<i32>, pub fit_qty: Vec<i32> }
impl Fitting { fn from_row(r: &Row) -> Self { Self { fitting_id: r.get("fitting_id"), owner_id: r.get("owner_id"), name: r.get("name"), description: r.get("description"), ship_type_id: r.get("ship_type_id"), fit_types: r.get("fit_types"), fit_flags: r.get("fit_flags"), fit_qty: r.get("fit_qty") } } }
pub(crate) struct Blueprint { pub item_id: i64, pub type_id: i32, pub owner_id: i64, pub location_id: i64, pub flag_id: i32, pub quantity: i32, pub singleton: bool, pub location_type_id: Option<i32>, pub location_parent_id: Option<i64>, pub character_system_id: Option<i64>, pub material_efficiency: i64, pub time_efficiency: i64, pub runs: i64, pub job_id: Option<i64> }
impl Blueprint { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), type_id: r.get("type_id"), owner_id: r.get("owner_id"), location_id: r.get("location_id"), flag_id: r.get("flag_id"), quantity: r.get("quantity"), singleton: r.get("singleton"), location_type_id: r.get("location_type_id"), location_parent_id: r.get("location_parent_id"), character_system_id: r.get("character_system_id"), material_efficiency: r.get("material_efficiency"), time_efficiency: r.get("time_efficiency"), runs: r.get("runs"), job_id: r.get("job_id") } } }

pub(crate) async fn assemble_sources(ex: &(impl Executor + ?Sized), sources: &BTreeMap<i64, (Item, Option<i32>)>, owner: i64, name: &Option<String>) -> Result<bool, Error> {
    for (id, (item, remaining)) in sources {
        let Some(remaining) = remaining else { continue };
        let changed = if *remaining == 0 {
            ex.execute("UPDATE game.items SET quantity = CASE WHEN quantity > 1 THEN 1 ELSE quantity END, singleton = true, name = COALESCE($7, name) WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = $4 AND type_id = $5 AND quantity = $6 AND NOT singleton", &[id, &owner, &item.location_id, &item.flag_id, &item.type_id, &item.quantity, name]).await?
        } else {
            ex.execute("UPDATE game.items SET quantity = $7 WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = $4 AND type_id = $5 AND quantity = $6 AND quantity > 1 AND NOT singleton", &[id, &owner, &item.location_id, &item.flag_id, &item.type_id, &item.quantity, remaining]).await?
        };
        if changed == 0 { return Ok(false); }
    }
    Ok(true)
}
pub(crate) async fn insert_assembled(ex: &(impl Executor + ?Sized), item: &Item, owner: i64, name: &Option<String>) -> Result<i64, Error> {
    Ok(ItemId::from_row(&ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, name)
                             VALUES ($1, $2, $3, $4, 1, true, $5) RETURNING item_id", &[&item.type_id, &owner, &item.location_id, &item.flag_id, name]).await?).item_id)
}
pub(crate) async fn lock_children(ex: &(impl Executor + ?Sized), parent: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT item_id FROM game.items WHERE location_id = $1 ORDER BY item_id FOR UPDATE", &[&parent]).await?
        .iter().map(|row| ItemId::from_row(row).item_id).collect())
}
pub(crate) async fn repackage(ex: &(impl Executor + ?Sized), item: &Item) -> Result<u64, Error> {
    let row = ex.query_one("WITH changed AS (UPDATE game.items SET singleton = false, online = false, name = NULL WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = $4 AND type_id = $5 AND quantity = $6 AND singleton = $7 AND singleton AND flag_id = $8 RETURNING item_id, owner_id, singleton), removed AS (DELETE FROM game.ship_skin_applications a USING changed c WHERE a.ship_item_id = c.item_id AND (a.character_id <> c.owner_id OR NOT c.singleton)) SELECT EXISTS (SELECT 1 FROM changed) AS matched", &[&item.item_id, &item.owner_id, &item.location_id, &item.flag_id, &item.type_id, &item.quantity, &item.singleton, &FLAG_HANGAR]).await?;
    Ok(u64::from(Matched::from_row(&row).matched))
}
pub(crate) async fn destroy_upgrade(ex: &(impl Executor + ?Sized), item: &Item) -> Result<bool, Error> {
    Ok(ex.execute("DELETE FROM game.items i WHERE i.item_id = $1 AND i.owner_id = $2 AND i.location_id = $3 AND i.flag_id = $4 AND i.type_id = $5 AND i.quantity = $6 AND i.singleton = $7 AND i.jump_clone_id IS NULL AND NOT EXISTS (SELECT 1 FROM game.items child WHERE child.location_id = i.item_id)", &[&item.item_id, &item.owner_id, &item.location_id, &item.flag_id, &item.type_id, &item.quantity, &item.singleton]).await? != 0)
}
const COLS: &str = "fitting_id, owner_id, name, description, ship_type_id, fit_types, fit_flags, fit_qty";
pub(crate) async fn fittings(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<Fitting>, Error> {
    Ok(ex.query(&format!("SELECT {COLS} FROM game.fittings WHERE owner_id = $1 ORDER BY fitting_id"), &[&owner]).await?.iter().map(Fitting::from_row).collect())
}
pub(crate) async fn save(ex: &(impl Executor + ?Sized), owner: i64, name: &str, description: &str, ship_type: i32, types: &[i32], flags: &[i32], qty: &[i32]) -> Result<i64, Error> {
    Ok(FittingId::from_row(&ex.query_one("INSERT INTO game.fittings (owner_id, name, description, ship_type_id, fit_types, fit_flags, fit_qty)
                             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING fitting_id", &[&owner, &name, &description, &ship_type, &types, &flags, &qty]).await?).fitting_id)
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), owner: i64, id: i64) -> Result<u64, Error> { ex.execute("DELETE FROM game.fittings WHERE owner_id = $1 AND fitting_id = $2", &[&owner, &id]).await }
pub(crate) async fn delete_many(ex: &(impl Executor + ?Sized), owner: i64, ids: &[i64]) -> Result<u64, Error> { ex.execute("DELETE FROM game.fittings WHERE owner_id = $1 AND fitting_id = ANY($2)", &[&owner, &ids]).await }
pub(crate) async fn rename(ex: &(impl Executor + ?Sized), id: i64, owner: i64, name: &str, description: &str) -> Result<u64, Error> { ex.execute("UPDATE game.fittings SET name = $3, description = $4 WHERE fitting_id = $1 AND owner_id = $2", &[&id, &owner, &name, &description]).await }
pub(crate) async fn blueprints(ex: &(impl Executor + ?Sized), owner: i64, item: Option<i64>) -> Result<Vec<Blueprint>, Error> {
    let statuses = [INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY];
    Ok(ex.query("SELECT i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton,
                    loc.type_id AS location_type_id, loc.location_id AS location_parent_id, ch.solar_system_id AS character_system_id,
                    COALESCE(b.material_efficiency, 0)::bigint AS material_efficiency, COALESCE(b.time_efficiency, 0)::bigint AS time_efficiency,
                    COALESCE(b.runs, $3::int)::bigint AS runs,
                    (SELECT j.job_id FROM game.industry_jobs j WHERE j.blueprint_id = i.item_id AND j.status = ANY($4::int[]) LIMIT 1) AS job_id
             FROM game.items i
             LEFT JOIN game.items loc ON loc.item_id = i.location_id
             LEFT JOIN game.characters ch ON ch.character_id = i.owner_id
             LEFT JOIN game.blueprints b ON b.item_id = i.item_id
             WHERE i.owner_id = $1 AND ($2::bigint IS NULL OR i.item_id = $2) AND i.location_id > 0", &[&owner, &item, &BLUEPRINT_INFINITE_RUNS, &statuses.as_slice()]).await?.iter().map(Blueprint::from_row).collect())
}
