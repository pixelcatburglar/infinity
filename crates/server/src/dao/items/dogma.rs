//! Online module state, active-item dogma overrides and condition checkpoints.
//! Uses game.items and game.item_attributes; dormant clone overrides belong to
//! the clone custody DAO. There is no direct client mirror: callers own policy,
//! transaction boundaries, checkpoint serialization and notifications.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct OnlineModule { pub item_id: i64 }
impl OnlineModule { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn online(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<OnlineModule>, Error> { Ok(ex.query("SELECT item_id FROM game.items WHERE location_id = $1 AND online", &[&ship]).await?.iter().map(OnlineModule::from_row).collect()) }

/// A persisted override's value and cargo-hold mode as one row of state.
/// `cargo_unlimited` only has meaning on a hull's capacity override; finite
/// writers always store `false`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverrideState {
    pub value: f64,
    pub cargo_unlimited: bool,
}

pub(crate) struct AttributeOverride {
    pub item_id: i64,
    pub attribute_id: i32,
    pub value: f64,
    pub updated_at: i64,
    pub cargo_unlimited: bool,
}

impl AttributeOverride {
    pub(super) fn from_row(r: &Row) -> Self {
        Self { item_id: r.get("item_id"), attribute_id: r.get("attribute_id"), value: r.get("value"),
            updated_at: r.get("updated_at"), cargo_unlimited: r.get("cargo_unlimited") }
    }

    /// The row's complete comparable presence/value/mode state.
    pub(crate) fn state(&self) -> OverrideState {
        OverrideState { value: self.value, cargo_unlimited: self.cargo_unlimited }
    }
}

pub(crate) async fn overrides_for(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<AttributeOverride>, Error> {
    Ok(ex.query("SELECT a.item_id, a.attribute_id, a.value, a.updated_at, a.cargo_unlimited FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE a.item_id = ANY($1) AND i.jump_clone_id IS NULL ORDER BY a.item_id, a.attribute_id", &[&ids]).await?.iter().map(AttributeOverride::from_row).collect())
}

pub(crate) async fn overrides(ex: &(impl Executor + ?Sized), item: i64) -> Result<Vec<AttributeOverride>, Error> {
    overrides_for(ex, &[item]).await
}

pub(crate) async fn set_override(ex: &(impl Executor + ?Sized), item: i64, attribute: i32, value: f64) -> Result<bool, Error> {
    Ok(ex.execute("WITH target AS (SELECT item_id FROM game.items WHERE item_id = $1 AND singleton AND jump_clone_id IS NULL FOR UPDATE) INSERT INTO game.item_attributes (item_id, attribute_id, value, cargo_unlimited) SELECT item_id, $2, $3, false FROM target ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value, cargo_unlimited = false", &[&item, &attribute, &value]).await? != 0)
}

/// Write a finite override only if its previously observed state has not changed.
/// `old` is the complete presence/value/mode state: `None` inserts only when no
/// row exists yet, and `Some` updates only when the row still carries both
/// persisted fields; a vanished or rewritten row is never recreated.
pub(crate) async fn set_override_if(ex: &(impl Executor + ?Sized), item: i64, attribute: i32, old: Option<OverrideState>, value: f64) -> Result<bool, Error> {
    match old {
        None => Ok(ex.execute("WITH target AS (SELECT item_id FROM game.items WHERE item_id = $1 AND singleton AND jump_clone_id IS NULL FOR UPDATE) INSERT INTO game.item_attributes (item_id, attribute_id, value, cargo_unlimited) SELECT item_id, $2, $3, false FROM target ON CONFLICT (item_id, attribute_id) DO NOTHING", &[&item, &attribute, &value]).await? != 0),
        Some(state) => Ok(ex.execute("WITH target AS (SELECT item_id FROM game.items WHERE item_id = $1 AND singleton AND jump_clone_id IS NULL FOR UPDATE) UPDATE game.item_attributes SET value = $4, cargo_unlimited = false FROM target WHERE game.item_attributes.item_id = $1 AND game.item_attributes.attribute_id = $2 AND game.item_attributes.value = $3 AND game.item_attributes.cargo_unlimited = $5 AND target.item_id = $1", &[&item, &attribute, &state.value, &value, &state.cargo_unlimited]).await? != 0),
    }
}

/// Enable unlimited cargo on the hull's capacity override, preserving the real
/// finite base of an existing row. Repeated writes are no-ops.
pub(crate) async fn set_cargo_unlimited(ex: &(impl Executor + ?Sized), item: i64, base: f64) -> Result<bool, Error> {
    Ok(ex.execute("WITH target AS (SELECT item_id FROM game.items WHERE item_id = $1 AND singleton AND jump_clone_id IS NULL FOR UPDATE) INSERT INTO game.item_attributes (item_id, attribute_id, value, cargo_unlimited) SELECT item_id, $2, $3, true FROM target ON CONFLICT (item_id, attribute_id) DO UPDATE SET cargo_unlimited = true WHERE NOT game.item_attributes.cargo_unlimited", &[&item, &infinity_core::consts::dogma::ATTR_CAPACITY, &base]).await? != 0)
}

/// A hull row and its persisted override state for one attribute, read after
/// the caller has locked the hull row.
pub(crate) struct HullOverride {
    pub item: super::loading::GameItem,
    pub state: Option<OverrideState>,
}

/// Join the active hull with its override row; a missing override stays `None`.
/// This function does not lock the hull row itself.
pub(crate) async fn hull_override(ex: &(impl Executor + ?Sized), item: i64, attribute: i32) -> Result<Option<HullOverride>, Error> {
    Ok(ex.query_opt("SELECT i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info, a.value AS override_value, a.cargo_unlimited AS override_unlimited FROM game.items i LEFT JOIN game.item_attributes a ON a.item_id = i.item_id AND a.attribute_id = $2 WHERE i.item_id = $1 AND i.jump_clone_id IS NULL", &[&item, &attribute]).await?.map(|row| HullOverride {
        item: super::loading::GameItem::from_row(&row),
        state: row.get::<_, Option<f64>>("override_value").map(|value| OverrideState { value, cargo_unlimited: row.get::<_, Option<bool>>("override_unlimited").unwrap_or(false) }),
    }))
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
             ON CONFLICT (item_id, attribute_id) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at, cargo_unlimited = false RETURNING item_id) \
         SELECT item_id FROM written GROUP BY item_id HAVING count(*) = 3 ORDER BY item_id",
        &[&location, &items, &shields, &armors, &hulls, &times, &&ids[..]],
    ).await?;
    Ok(written.iter().map(|row| row.get("item_id")).collect())
}
#[cfg(all(test, feature = "tests-storage"))] mod tests;
