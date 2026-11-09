//! Owner-bound bay settings in `game.ship_configuration` and guarded hull selection
//! in `game.items` and `game.characters`. The ship service owns client policy;
//! transactions, SDE type checks and notifications are not handled here.
use crate::db::{self, executor::Executor};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Settings {
    pub owner_id: i64,
    pub fleet_hangar_fleet: bool,
    pub fleet_hangar_corp: bool,
    pub maintenance_fleet: bool,
    pub maintenance_corp: bool,
}
impl Settings {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { owner_id: r.get("owner_id"), fleet_hangar_fleet: r.get("fleet_hangar_fleet"),
            fleet_hangar_corp: r.get("fleet_hangar_corp"), maintenance_fleet: r.get("maintenance_fleet"),
            maintenance_corp: r.get("maintenance_corp") }
    }
}
#[derive(Clone, Copy, Default)]
pub(crate) struct Patch {
    pub fleet_hangar_fleet: Option<bool>,
    pub fleet_hangar_corp: Option<bool>,
    pub maintenance_fleet: Option<bool>,
    pub maintenance_corp: Option<bool>,
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Option<Settings>, db::Error> {
    Ok(ex.query_opt("SELECT owner_id, fleet_hangar_fleet, fleet_hangar_corp, maintenance_fleet, maintenance_corp FROM game.ship_configuration WHERE ship_item_id=$1", &[&ship]).await?.as_ref().map(Settings::from_row))
}

pub(crate) struct ShipSettings {
    pub ship_item_id: i64,
    pub settings: Settings,
}

pub(crate) async fn get_many(ex: &(impl Executor + ?Sized), ships: &[i64]) -> Result<Vec<ShipSettings>, db::Error> {
    Ok(ex.query("SELECT ship_item_id, owner_id, fleet_hangar_fleet, fleet_hangar_corp, maintenance_fleet, maintenance_corp FROM game.ship_configuration WHERE ship_item_id=ANY($1)", &[&ships]).await?.iter().map(|row| ShipSettings {
        ship_item_id: row.get("ship_item_id"), settings: Settings::from_row(row),
    }).collect())
}
/// Caller holds the hull lock until commit. Prior owners' unspecified switches reset.
pub(crate) async fn patch(ex: &(impl Executor + ?Sized), ship: i64, owner: i64, patch: &Patch) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.ship_configuration AS cfg (ship_item_id, owner_id, fleet_hangar_fleet, fleet_hangar_corp, maintenance_fleet, maintenance_corp) SELECT item_id, owner_id, COALESCE($3,false), COALESCE($4,false), COALESCE($5,false), COALESCE($6,false) FROM game.items WHERE item_id=$1 AND owner_id=$2 ON CONFLICT (ship_item_id) DO UPDATE SET owner_id=EXCLUDED.owner_id, fleet_hangar_fleet=COALESCE($3, CASE WHEN cfg.owner_id=$2 THEN cfg.fleet_hangar_fleet ELSE false END), fleet_hangar_corp=COALESCE($4, CASE WHEN cfg.owner_id=$2 THEN cfg.fleet_hangar_corp ELSE false END), maintenance_fleet=COALESCE($5, CASE WHEN cfg.owner_id=$2 THEN cfg.maintenance_fleet ELSE false END), maintenance_corp=COALESCE($6, CASE WHEN cfg.owner_id=$2 THEN cfg.maintenance_corp ELSE false END)", &[&ship, &owner, &patch.fleet_hangar_fleet, &patch.fleet_hangar_corp, &patch.maintenance_fleet, &patch.maintenance_corp]).await? == 1)
}
/// Serialize against boarding and ownership/location changes; return the locked type.
pub(crate) async fn lock_selected(ex: &(impl Executor + ?Sized), character: i64, user: i64, ship: i64, station: Option<i64>, system: i64, flag: i32) -> Result<Option<i32>, db::Error> {
    if ex.query_opt("SELECT character_id FROM game.characters WHERE character_id=$1 AND user_id=$2 AND ship_id=$3 AND NOT deleted AND station_id IS NOT DISTINCT FROM $4 AND solar_system_id=$5 FOR UPDATE", &[&character, &user, &ship, &station, &system]).await?.is_none() { return Ok(None); }
    super::ship::lock_owned_at(ex, ship, character, station.unwrap_or(system), flag).await
}

#[cfg(test)] mod tests;
