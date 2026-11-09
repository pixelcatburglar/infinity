//! Online module state, active-item dogma overrides and condition checkpoints.
//! Uses game.items and game.item_attributes; dormant clone overrides belong to
//! the clone custody DAO. There is no direct client mirror: callers own policy,
//! transaction boundaries, checkpoint serialization and notifications.
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


/// Persist complete condition snapshots for singleton items still at the expected location.
/// Returned IDs are the items whose three attributes were written.
pub(crate) async fn save_conditions(ex: &(impl Executor + ?Sized), location: i64, ids: &[i32; 3], rows: &[(i64, [f64; 3], i64)]) -> Result<Vec<i64>, Error> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::with_capacity(rows.len());
    let mut shields = Vec::with_capacity(rows.len());
    let mut armors = Vec::with_capacity(rows.len());
    let mut hulls = Vec::with_capacity(rows.len());
    let mut times = Vec::with_capacity(rows.len());
    for &(item, values, at) in rows {
        items.push(item);
        shields.push(values[0]);
        armors.push(values[1]);
        hulls.push(values[2]);
        times.push(at);
    }
    let written = ex.query(
        "WITH input AS (SELECT * FROM unnest($2::bigint[], $3::float8[], $4::float8[], $5::float8[], $6::bigint[]) AS r(item_id, shield, armor, hull, shield_at)), \
         target AS (SELECT i.item_id, r.shield, r.armor, r.hull, r.shield_at FROM game.items i JOIN input r ON r.item_id = i.item_id WHERE i.location_id = $1 AND i.singleton AND i.jump_clone_id IS NULL ORDER BY i.item_id FOR UPDATE OF i), \
         written AS (INSERT INTO game.item_attributes (item_id, attribute_id, value, updated_at) \
             SELECT t.item_id, a.id, CASE a.n WHEN 1 THEN t.shield WHEN 2 THEN t.armor ELSE t.hull END, CASE WHEN a.n = 1 THEN t.shield_at ELSE 0 END \
             FROM target t CROSS JOIN unnest($7::integer[]) WITH ORDINALITY AS a(id, n) ORDER BY t.item_id, a.id \
             ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at RETURNING item_id) \
         SELECT item_id FROM written GROUP BY item_id HAVING count(*) = 3 ORDER BY item_id",
        &[&location, &items, &shields, &armors, &hulls, &times, &&ids[..]],
    ).await?;
    Ok(written.iter().map(|row| row.get("item_id")).collect())
}
#[cfg(test)] mod tests;
