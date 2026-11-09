//! Loaded charge and weapon bank lookups in `game.module_charges` and `game.weapon_banks`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Charge effects, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct Charge { pub flag_id: i32, pub type_id: i32, pub quantity: i64 }
impl Charge { pub(super) fn from_row(r: &Row) -> Self { Self { flag_id: r.get("flag_id"), type_id: r.get("type_id"), quantity: r.get("quantity") } } }
pub(crate) async fn positive(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Charge>, Error> { Ok(ex.query("SELECT flag_id, type_id, quantity FROM game.module_charges WHERE ship_id = $1 AND quantity > 0", &[&ship]).await?.iter().map(Charge::from_row).collect()) }

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
pub(crate) async fn slaves(ex: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<Vec<Slave>, Error> { Ok(ex.query("SELECT b.slave_id FROM game.weapon_banks b JOIN game.items i ON i.item_id = b.slave_id WHERE b.ship_id = $1 AND b.master_id = $2 ORDER BY i.flag_id, i.item_id", &[&ship, &master]).await?.iter().map(Slave::from_row).collect()) }
pub(crate) async fn banks(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<Bank>, Error> { Ok(ex.query("SELECT b.master_id, b.slave_id FROM game.weapon_banks b JOIN game.items i ON i.item_id = b.slave_id WHERE b.ship_id = $1 ORDER BY i.flag_id, i.item_id", &[&ship]).await?.iter().map(Bank::from_row).collect()) }
pub(crate) async fn bank_of(ex: &(impl Executor + ?Sized), ship: i64, item: i64) -> Result<i64, Error> {
    Ok(ex.query_opt("SELECT master_id FROM game.weapon_banks WHERE ship_id = $1 AND slave_id = $2", &[&ship, &item]).await?.as_ref().map(Master::from_row).map_or(item, |r| r.master_id))
}