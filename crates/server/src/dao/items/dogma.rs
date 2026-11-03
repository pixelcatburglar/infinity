//! Online module state and active-item dogma overrides in the game database.
//! Dormant clone overrides are accessed by the clone custody DAO. This DAO has
//! no direct client mirror; callers own policy, transactions and notifications.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct OnlineModule { pub item_id: i64 }
impl OnlineModule { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn online(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<OnlineModule>, Error> { Ok(ex.query("SELECT item_id FROM game.items WHERE location_id = $1 AND online", &[&ship]).await?.iter().map(OnlineModule::from_row).collect()) }

pub(crate) struct AttributeOverride {
    pub item_id: i64,
    pub attribute_id: i32,
    pub value: f64,
    pub updated_at: i64,
}

impl AttributeOverride {
    pub(super) fn from_row(r: &Row) -> Self {
        Self { item_id: r.get("item_id"), attribute_id: r.get("attribute_id"), value: r.get("value"), updated_at: r.get("updated_at") }
    }
}

pub(crate) async fn overrides_for(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<AttributeOverride>, Error> {
    Ok(ex.query("SELECT a.item_id, a.attribute_id, a.value, a.updated_at FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE a.item_id = ANY($1) AND i.jump_clone_id IS NULL ORDER BY a.item_id, a.attribute_id", &[&ids]).await?.iter().map(AttributeOverride::from_row).collect())
}

pub(crate) async fn overrides(ex: &(impl Executor + ?Sized), item: i64) -> Result<Vec<AttributeOverride>, Error> {
    overrides_for(ex, &[item]).await
}

pub(crate) async fn set_override(ex: &(impl Executor + ?Sized), item: i64, attribute: i32, value: f64) -> Result<bool, Error> {
    Ok(ex.execute("WITH target AS (SELECT item_id FROM game.items WHERE item_id = $1 AND singleton AND jump_clone_id IS NULL FOR UPDATE) INSERT INTO game.item_attributes (item_id, attribute_id, value) SELECT item_id, $2, $3 FROM target ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value", &[&item, &attribute, &value]).await? != 0)
}

/// Write an override only if its previously observed value has not changed.
pub(crate) async fn set_override_if(ex: &(impl Executor + ?Sized), item: i64, attribute: i32, old: Option<f64>, value: f64) -> Result<bool, Error> {
    Ok(ex.execute("WITH target AS (SELECT item_id FROM game.items WHERE item_id = $1 AND singleton AND jump_clone_id IS NULL FOR UPDATE) INSERT INTO game.item_attributes (item_id, attribute_id, value) SELECT item_id, $2, $4 FROM target WHERE $3::float8 IS NULL OR EXISTS (SELECT 1 FROM game.item_attributes WHERE item_id = $1 AND attribute_id = $2 AND value = $3) ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value WHERE game.item_attributes.value = $3", &[&item, &attribute, &old, &value]).await? != 0)
}

pub(crate) async fn clear_override(ex: &(impl Executor + ?Sized), item: i64, attribute: i32) -> Result<(), Error> {
    ex.execute("DELETE FROM game.item_attributes a USING game.items i WHERE a.item_id = $1 AND a.attribute_id = $2 AND i.item_id = a.item_id AND i.jump_clone_id IS NULL", &[&item, &attribute]).await?;
    Ok(())
}

pub(crate) async fn clear_overrides(ex: &(impl Executor + ?Sized), item: i64) -> Result<(), Error> {
    ex.execute("DELETE FROM game.item_attributes a USING game.items i WHERE a.item_id = $1 AND i.item_id = a.item_id AND i.jump_clone_id IS NULL", &[&item]).await?;
    Ok(())
}


/// Atomically persist all ship layers in attribute-ID order.
pub(crate) async fn save_condition(ex: &(impl Executor + ?Sized), item: i64, ids: &[i32], values: &[f64], shield: i32, at: i64) -> Result<(), Error> {
    ex.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value, updated_at) SELECT $1, a.id, a.value, CASE WHEN a.id = $4 THEN $5::bigint ELSE 0 END FROM unnest($2::integer[], $3::double precision[]) AS a(id, value) JOIN game.items i ON i.item_id = $1 AND i.singleton AND i.jump_clone_id IS NULL ORDER BY a.id ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at", &[&item, &ids, &values, &shield, &at]).await?;
    Ok(())
}
