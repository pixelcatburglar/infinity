//! Asteroid field growth, depletion and rock persistence in `game.asteroid_fields`
//! and `game.asteroids`. This DAO has no direct client mirror or wire contract.
//! Mining and space simulation, transactions and notifications remain with callers.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Asteroid { pub id: i64, pub field: i64, pub type_id: i32, pub quantity: i64, pub radius: Option<f32>, pub x: f64, pub y: f64, pub z: f64 }
impl Asteroid { fn from_row(r: &Row) -> Self { Self { id: r.get("asteroid_id"), field: r.get("field_id"), type_id: r.get("type_id"), quantity: r.get("quantity"), radius: r.get("radius"), x: r.get("x"), y: r.get("y"), z: r.get("z") } } }
pub(crate) struct NewAsteroid { pub field: i64, pub type_id: i32, pub quantity: i64, pub radius: f32, pub pos: [f64; 3] }

/// Allocate IDs within the transaction attempt and associate returned rows by ID,
/// never by the unspecified order of INSERT RETURNING.
pub(crate) async fn insert_batch(ex: &(impl Executor + ?Sized), system: i64, rocks: &[NewAsteroid]) -> Result<Vec<Asteroid>, db::Error> {
    if rocks.is_empty() { return Ok(Vec::new()); }
    let fields: Vec<_> = rocks.iter().map(|r| r.field).collect();
    let types: Vec<_> = rocks.iter().map(|r| r.type_id).collect();
    let quantities: Vec<_> = rocks.iter().map(|r| r.quantity).collect();
    let radii: Vec<_> = rocks.iter().map(|r| r.radius).collect();
    let xs: Vec<_> = rocks.iter().map(|r| r.pos[0]).collect();
    let ys: Vec<_> = rocks.iter().map(|r| r.pos[1]).collect();
    let zs: Vec<_> = rocks.iter().map(|r| r.pos[2]).collect();
    Ok(ex.query("WITH supplied AS MATERIALIZED (
        SELECT nextval('game.asteroid_seq') AS asteroid_id, input.*
        FROM unnest($2::bigint[], $3::integer[], $4::bigint[], $5::real[],
                    $6::double precision[], $7::double precision[], $8::double precision[])
             WITH ORDINALITY AS input(field_id, type_id, quantity, radius, x, y, z, ord)
    ), inserted AS (
        INSERT INTO game.asteroids (asteroid_id, system_id, field_id, type_id, quantity, original_quantity, radius, x, y, z)
        SELECT asteroid_id, $1, field_id, type_id, quantity, quantity, radius, x, y, z FROM supplied
        RETURNING asteroid_id, field_id, type_id, quantity, radius, x, y, z
    )
    SELECT inserted.* FROM inserted JOIN supplied USING (asteroid_id) ORDER BY supplied.ord",
        &[&system, &fields, &types, &quantities, &radii, &xs, &ys, &zs]).await?.iter().map(Asteroid::from_row).collect())
}
pub(crate) async fn load(ex: &(impl Executor + ?Sized), system: i64) -> Result<Vec<Asteroid>, db::Error> { Ok(ex.query("SELECT asteroid_id, field_id, type_id, quantity, radius, x, y, z FROM game.asteroids WHERE system_id = $1 ORDER BY asteroid_id", &[&system]).await?.iter().map(Asteroid::from_row).collect()) }

/// Durable marker for one field; `depleted_at == None` includes initialized-empty fields.
pub(crate) struct FieldState {
    pub field: i64,
    pub system: i64,
    pub depleted_at: Option<i64>,
}

impl FieldState {
    fn from_row(row: &Row) -> Self {
        Self { field: row.get("field_id"), system: row.get("system_id"), depleted_at: row.get("depleted_at") }
    }
}
/// Marker and row classification; populated includes partially mined fields.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FieldStatus {
    NeverInitialized,
    InitializedEmpty,
    Populated,
    Depleted { depleted_at: i64 },
    WrongSystem { system: i64 },
}

pub(crate) fn field_status(marker: Option<&FieldState>, system: i64, populated: bool) -> FieldStatus {
    if let Some(marker) = marker {
        if marker.system != system { return FieldStatus::WrongSystem { system: marker.system }; }
    }
    if populated { return FieldStatus::Populated; }
    match marker {
        None => FieldStatus::NeverInitialized,
        Some(marker) => match marker.depleted_at {
            Some(depleted_at) => FieldStatus::Depleted { depleted_at },
            None => FieldStatus::InitializedEmpty,
        },
    }
}

/// Load system markers and globally keyed requested fields in one read.
pub(crate) async fn fields(ex: &(impl Executor + ?Sized), system: i64, requested: &[i64]) -> Result<Vec<FieldState>, db::Error> {
    Ok(ex.query("SELECT field_id, system_id, depleted_at FROM game.asteroid_fields WHERE system_id = $1 OR field_id = ANY($2)",
        &[&system, &requested]).await?.iter().map(FieldState::from_row).collect())
}

/// Read a globally keyed marker, including markers belonging to another system.
pub(crate) async fn field(ex: &(impl Executor + ?Sized), field: i64) -> Result<Option<FieldState>, db::Error> {
    Ok(ex.query_opt("SELECT field_id, system_id, depleted_at FROM game.asteroid_fields WHERE field_id = $1",
        &[&field]).await?.as_ref().map(FieldState::from_row))
}

/// Mark fields initialized without ever overwriting a different system's marker.
pub(crate) async fn grown_batch(ex: &(impl Executor + ?Sized), system: i64, fields: &[i64], now: i64) -> Result<bool, db::Error> {
    if fields.is_empty() { return Ok(true); }
    Ok(ex.execute("INSERT INTO game.asteroid_fields (field_id, system_id, grown_at)
        SELECT field_id, $1, $3 FROM unnest($2::bigint[]) AS input(field_id) ORDER BY field_id
        ON CONFLICT (field_id) DO UPDATE SET grown_at = EXCLUDED.grown_at, depleted_at = NULL
        WHERE game.asteroid_fields.system_id = EXCLUDED.system_id", &[&system, &fields, &now]).await? == fields.len() as u64)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Depletion {
    Deleted { field: i64, deadline: Option<i64> },
    AlreadyDepleted { field: i64, deadline: Option<i64> },
    AbsentWrongField,
}

/// Execute in the caller's transaction, under the existing settlement lock.
/// `regrowth` is present only for a belt and is expressed in FILETIME units.
/// `id` and `field` must have been captured from the same live actor. A missing
/// row with a matching initialized marker is a replay; only a depleted marker
/// can supply a deadline when no deletion was performed in this transaction.
pub(crate) async fn deplete_scoped(
    ex: &(impl Executor + ?Sized), system: i64, field: i64, id: i64, now: i64, regrowth: Option<i64>,
) -> Result<Depletion, db::Error> {
    let marker = self::field(ex, field).await?;
    if marker.as_ref().is_some_and(|marker| marker.system != system) {
        return Ok(Depletion::AbsentWrongField);
    }
    let row = ex.query_opt("SELECT system_id, field_id, quantity FROM game.asteroids WHERE asteroid_id = $1 FOR UPDATE", &[&id]).await?;
    let deleted = if let Some(row) = row {
        let owner: i64 = row.get("system_id");
        let owner_field: i64 = row.get("field_id");
        let quantity: i64 = row.get("quantity");
        if owner != system || owner_field != field || quantity > 0 {
            return Ok(Depletion::AbsentWrongField);
        }
        ex.execute("DELETE FROM game.asteroids WHERE asteroid_id = $1 AND system_id = $2 AND field_id = $3 AND quantity <= 0",
            &[&id, &system, &field]).await? == 1
    } else {
        if marker.is_none() {
            return Ok(Depletion::AbsentWrongField);
        }
        false
    };
    let remaining = ex.query_opt("SELECT asteroid_id FROM game.asteroids WHERE system_id = $1 AND field_id = $2 LIMIT 1",
        &[&system, &field]).await?.is_some();
    if deleted && remaining && marker.is_none() && !grown_batch(ex, system, &[field], now).await? {
        return Err(db::Error::Progress(anyhow::anyhow!("asteroid field {field} belongs to another system")));
    }
    let deadline = if !remaining && (deleted || marker.as_ref().and_then(|m| m.depleted_at).is_some()) {
        let depleted = marker.as_ref().and_then(|m| m.depleted_at).unwrap_or(now);
        let updated = ex.execute("INSERT INTO game.asteroid_fields (field_id, system_id, grown_at, depleted_at) VALUES ($1, $2, $3, $3)
            ON CONFLICT (field_id) DO UPDATE SET depleted_at = COALESCE(game.asteroid_fields.depleted_at, EXCLUDED.depleted_at)
            WHERE game.asteroid_fields.system_id = EXCLUDED.system_id", &[&field, &system, &depleted]).await?;
        if updated != 1 {
            return Err(db::Error::Progress(anyhow::anyhow!("asteroid field {field} belongs to another system")));
        }
        regrowth.map(|duration| depleted.saturating_add(duration))
    } else { None };
    Ok(if deleted { Depletion::Deleted { field, deadline } } else { Depletion::AlreadyDepleted { field, deadline } })
}
/// Lock a rock for an extraction in the caller's transaction.
pub(crate) async fn lock(ex: &(impl Executor + ?Sized), system: i64, id: i64, type_id: i32) -> Result<Option<i64>, db::Error> {
    Ok(ex.query_opt("SELECT quantity FROM game.asteroids WHERE system_id = $1 AND asteroid_id = $2 AND type_id = $3 FOR UPDATE", &[&system, &id, &type_id]).await?.map(|r| r.get("quantity")))
}
/// Decrement only the quantity observed under the lock.
pub(crate) async fn take(ex: &(impl Executor + ?Sized), id: i64, old: i64, taken: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.asteroids SET quantity = quantity - $3 WHERE asteroid_id = $1 AND quantity = $2 AND quantity >= $3", &[&id, &old, &taken]).await? == 1)
}

#[cfg(test)] mod tests;
