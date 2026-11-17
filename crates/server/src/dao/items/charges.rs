//! Loaded charge and weapon bank lookups in `game.module_charges` and `game.weapon_banks`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Charge effects, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct Charge { pub flag_id: i32, pub type_id: i32, pub quantity: i64 }
impl Charge { pub(super) fn from_row(r: &Row) -> Self { Self { flag_id: r.get("flag_id"), type_id: r.get("type_id"), quantity: r.get("quantity") } } }

/// One actor-captured absolute charge row. `incarnation` is in-process
/// identity only; SQL matches on `(ship_id, flag_id, type_id)`.
#[derive(Clone, Debug)]
pub(crate) struct LoadedWrite {
    pub ship: i64,
    pub module: i64,
    pub flag: i32,
    pub type_id: i32,
    pub incarnation: u64,
    pub quantity: i64,
}
/// The durable key of one loaded slot; `module` is the actor's item identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LoadedSlotKey {
    pub ship: i64,
    pub module: i64,
    pub flag: i32,
}
impl LoadedWrite {
    pub(crate) fn key(&self) -> LoadedSlotKey { LoadedSlotKey { ship: self.ship, module: self.module, flag: self.flag } }
}

pub(crate) struct Loaded { pub type_id: i32, pub quantity: i64 }
impl Loaded { pub(super) fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), quantity: r.get("quantity") } } }
pub(crate) struct Slave { pub slave_id: i64 }
impl Slave { pub(super) fn from_row(r: &Row) -> Self { Self { slave_id: r.get("slave_id") } } }
pub(crate) struct Bank { pub master_id: i64, pub slave_id: i64 }
impl Bank { fn from_row(r: &Row) -> Self { Self { master_id: r.get("master_id"), slave_id: r.get("slave_id") } } }
struct Master { master_id: i64 }
impl Master { fn from_row(r: &Row) -> Self { Self { master_id: r.get("master_id") } } }
pub(crate) async fn all(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Charge>, Error> { Ok(ex.query("SELECT flag_id, type_id, quantity FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?.iter().map(Charge::from_row).collect()) }
pub(crate) async fn loaded(ex: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<Option<Loaded>, Error> { Ok(ex.query_opt("SELECT type_id, quantity FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&ship, &flag]).await?.as_ref().map(Loaded::from_row)) }

/// Batch absolute write: one UPDATE for positive quantities, one DELETE for
/// emptied slots, both guarded by the captured type. Never inserts; returns
/// the keys of input rows the database actually matched.
pub(crate) async fn save_absolute(ex: &(impl Executor + ?Sized), rows: &[LoadedWrite]) -> Result<Vec<LoadedSlotKey>, Error> {
    let mut matched = std::collections::HashSet::new();
    let kept: Vec<_> = rows.iter().filter(|r| r.quantity > 0).collect();
    if !kept.is_empty() {
        let ships: Vec<i64> = kept.iter().map(|r| r.ship).collect();
        let flags: Vec<i32> = kept.iter().map(|r| r.flag).collect();
        let types: Vec<i32> = kept.iter().map(|r| r.type_id).collect();
        let quantities: Vec<i64> = kept.iter().map(|r| r.quantity).collect();
        for row in ex.query(
            "UPDATE game.module_charges AS m SET quantity = u.quantity FROM UNNEST($1::bigint[], $2::integer[], $3::integer[], $4::bigint[]) AS u(ship_id, flag_id, type_id, quantity) WHERE m.ship_id = u.ship_id AND m.flag_id = u.flag_id AND m.type_id = u.type_id RETURNING m.ship_id, m.flag_id, m.type_id",
            &[&ships, &flags, &types, &quantities]).await? {
            matched.insert((row.get::<_, i64>("ship_id"), row.get::<_, i32>("flag_id"), row.get::<_, i32>("type_id")));
        }
    }
    let dead: Vec<_> = rows.iter().filter(|r| r.quantity <= 0).collect();
    if !dead.is_empty() {
        let ships: Vec<i64> = dead.iter().map(|r| r.ship).collect();
        let flags: Vec<i32> = dead.iter().map(|r| r.flag).collect();
        let types: Vec<i32> = dead.iter().map(|r| r.type_id).collect();
        for row in ex.query(
            "DELETE FROM game.module_charges AS m USING UNNEST($1::bigint[], $2::integer[], $3::integer[]) AS u(ship_id, flag_id, type_id) WHERE m.ship_id = u.ship_id AND m.flag_id = u.flag_id AND m.type_id = u.type_id RETURNING m.ship_id, m.flag_id, m.type_id",
            &[&ships, &flags, &types]).await? {
            matched.insert((row.get::<_, i64>("ship_id"), row.get::<_, i32>("flag_id"), row.get::<_, i32>("type_id")));
        }
    }
    Ok(rows.iter().filter(|r| matched.contains(&(r.ship, r.flag, r.type_id))).map(LoadedWrite::key).collect())
}
pub(crate) async fn slaves(ex: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<Vec<Slave>, Error> { Ok(ex.query("SELECT b.slave_id FROM game.weapon_banks b JOIN game.items i ON i.item_id = b.slave_id WHERE b.ship_id = $1 AND b.master_id = $2 ORDER BY i.flag_id, i.item_id", &[&ship, &master]).await?.iter().map(Slave::from_row).collect()) }
pub(crate) async fn banks(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Bank>, Error> { Ok(ex.query("SELECT b.master_id, b.slave_id FROM game.weapon_banks b JOIN game.items i ON i.item_id = b.slave_id WHERE b.ship_id = $1 ORDER BY i.flag_id, i.item_id", &[&ship]).await?.iter().map(Bank::from_row).collect()) }
pub(crate) async fn bank_of(ex: &(impl Executor + ?Sized), ship: i64, item: i64) -> Result<i64, Error> {
    Ok(ex.query_opt("SELECT master_id FROM game.weapon_banks WHERE ship_id = $1 AND slave_id = $2", &[&ship, &item]).await?.as_ref().map(Master::from_row).map_or(item, |r| r.master_id))
}