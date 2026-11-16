//! Typed, immutable sovereignty development-index definitions.
//! The closed `Compile` operation in `content/sovereignty_indices/policy.lua` maps the
//! authored scalars in `data/policy.json` and the extracted `sovereigntyindices`
//! dataset onto the parameters the server executes mechanically; there is no client
//! analogue. Retained packages keep the authored JSON, the dataset bytes and the
//! compiled definitions so a restart never needs the deployment files again.

use mlua::{Lua, Table, Value};
use serde::{Deserialize, Serialize};
use infinity_core::consts::FILETIME_SECOND;
use crate::{config::lua::LuaConfig, hash::sha256_hex};
use super::policy;

/// Registration namespace of the development-index package.
pub(crate) const NAMESPACE: &str = "sovereignty_indices";
/// Client `devIndexUtil.GetDevIndexLevels` produces `totalLevels + 1` bands per axis.
const LEVEL_BANDS: usize = 6;
/// Client `devIndexUtil.timeIndexLevels` holds five strategic age thresholds.
const STRATEGIC_THRESHOLDS: usize = 5;
/// Bound on the retained authored JSON and the used dataset envelope bytes.
const INDICES_SIZE_FACTOR: usize = 8;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LevelBand {
    pub level: i64,
    pub min_points: f64,
    pub max_points: f64,
    pub modifier: f64,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LevelBonus {
    pub level: i64,
    pub bonus: f64,
}

/// Additive occupancy parameters mirroring `occupancyCalculator.py`;
/// the defense multiplier is the clamped bonus total, its reciprocal is occupancy.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Occupancy {
    pub base: f64,
    pub min: f64,
    pub max: f64,
    pub capital: f64,
    pub military: Vec<LevelBonus>,
    pub industrial: Vec<LevelBonus>,
    pub strategic: Vec<LevelBonus>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Definitions {
    pub military_attribute: i64,
    pub industrial_attribute: i64,
    pub strategic_attribute: i64,
    pub military: Vec<LevelBand>,
    pub industrial: Vec<LevelBand>,
    pub strategic_seconds: Vec<i64>,
    pub military_points_per_death: f64,
    pub industrial_points_per_m3: f64,
    pub decay_points: f64,
    pub decay_interval_ticks: i64,
    pub occupancy: Occupancy,
}

/// Authored JSON identity and point scalars; thresholds and occupancy come from
/// the deployment dataset, never from this file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authored {
    version: String,
    source: String,
    military_points_per_death: f64,
    industrial_points_per_m3: f64,
    decay_points: f64,
    decay_interval_seconds: i64,
}

/// The `sovereigntyindices` envelope retains a single anonymous record.
#[derive(Deserialize)]
struct DatasetEnvelope { records: Vec<DatasetRecord> }
#[derive(Deserialize)]
struct DatasetRecord { key: String, data: serde_json::Value }

/// Everything needed to rebuild this package without deployment files.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndicesData {
    authored: Vec<u8>,
    dataset: Vec<u8>,
    compiled: Definitions,
}

#[derive(Serialize)]
struct IndicesDataRef<'a> {
    authored: &'a [u8],
    dataset: &'a [u8],
    compiled: &'a Definitions,
}

fn authored(bytes: &[u8]) -> mlua::Result<Authored> {
    let authored: Authored = serde_json::from_slice(bytes).map_err(mlua::Error::external)?;
    let finite_positive = [authored.military_points_per_death, authored.industrial_points_per_m3, authored.decay_points];
    if authored.version.is_empty() || authored.source.is_empty()
        || finite_positive.iter().any(|value| !value.is_finite() || *value <= 0.0)
        || authored.decay_interval_seconds <= 0
        || authored.decay_interval_seconds > i64::MAX / FILETIME_SECOND {
        return Err(mlua::Error::runtime("invalid indices policy data"));
    }
    Ok(authored)
}

/// The authored package version; the full package version appends the dataset digest.
pub(super) fn authored_version(bytes: &[u8]) -> mlua::Result<String> {
    Ok(authored(bytes)?.version)
}

fn dataset(bytes: &[u8]) -> mlua::Result<serde_json::Value> {
    let envelope: DatasetEnvelope = serde_json::from_slice(bytes).map_err(mlua::Error::external)?;
    let [record] = envelope.records.as_slice() else { return Err(mlua::Error::runtime("indices dataset must hold one record")); };
    if !record.key.is_empty() || !record.data.is_object() {
        return Err(mlua::Error::runtime("invalid indices dataset record"));
    }
    Ok(record.data.clone())
}

fn field(table: &Table, key: &str) -> mlua::Result<Value> {
    match table.get::<Value>(key)? {
        Value::Nil => Err(mlua::Error::runtime(format!("missing indices field {key}"))),
        value => Ok(value),
    }
}

fn keys(table: &Table, allowed: &[&str]) -> mlua::Result<()> {
    for pair in table.clone().pairs::<String, Value>() {
        let (key, _) = pair?;
        if !allowed.contains(&key.as_str()) {
            return Err(mlua::Error::runtime(format!("unknown indices field {key}")));
        }
    }
    Ok(())
}

fn table_at(table: &Table, key: &str) -> mlua::Result<Table> {
    match field(table, key)? {
        Value::Table(value) => Ok(value),
        _ => Err(mlua::Error::runtime(format!("indices field {key} must be a table"))),
    }
}

fn integer_at(table: &Table, key: &str) -> mlua::Result<i64> {
    match field(table, key)? {
        Value::Integer(value) => Ok(value),
        _ => Err(mlua::Error::runtime(format!("indices field {key} must be an integer"))),
    }
}

fn number_at(table: &Table, key: &str) -> mlua::Result<f64> {
    let value = match field(table, key)? {
        Value::Integer(value) => value as f64,
        Value::Number(value) => value,
        _ => return Err(mlua::Error::runtime(format!("indices field {key} must be a number"))),
    };
    if !value.is_finite() { return Err(mlua::Error::runtime(format!("indices field {key} must be finite"))); }
    Ok(value)
}

fn sequence(list: &Table, name: &str) -> mlua::Result<Vec<Table>> {
    let mut items = Vec::new();
    for pair in list.clone().pairs::<Value, Value>() {
        let (index, value) = pair?;
        let (Value::Integer(index), Value::Table(item)) = (index, value) else {
            return Err(mlua::Error::runtime(format!("invalid indices list {name}")));
        };
        if index < 1 { return Err(mlua::Error::runtime(format!("invalid indices list {name}"))); }
        items.push((index as usize, item));
    }
    items.sort_by_key(|(index, _)| *index);
    if items.iter().enumerate().any(|(position, (index, _))| *index != position + 1) {
        return Err(mlua::Error::runtime(format!("indices list {name} is not a sequence")));
    }
    Ok(items.into_iter().map(|(_, item)| item).collect())
}

fn band(item: &Table) -> mlua::Result<LevelBand> {
    keys(item, &["level", "min_points", "max_points", "modifier"])?;
    Ok(LevelBand {
        level: integer_at(item, "level")?,
        min_points: number_at(item, "min_points")?,
        max_points: number_at(item, "max_points")?,
        modifier: number_at(item, "modifier")?,
    })
}

fn bonus(item: &Table) -> mlua::Result<LevelBonus> {
    keys(item, &["level", "bonus"])?;
    Ok(LevelBonus { level: integer_at(item, "level")?, bonus: number_at(item, "bonus")? })
}

fn bands(axis: &Table, name: &str) -> mlua::Result<(Vec<LevelBand>, f64)> {
    let scalar = if name == "military" { "points_per_death" } else { "points_per_m3" };
    keys(axis, &["bands", scalar])?;
    let bands = sequence(&table_at(axis, "bands")?, name)?.iter().map(band).collect::<mlua::Result<Vec<_>>>()?;
    Ok((bands, number_at(axis, scalar)?))
}

fn occupancy(table: &Table) -> mlua::Result<Occupancy> {
    keys(table, &["base", "min", "max", "capital", "military", "industrial", "strategic"])?;
    let bonuses = |key: &str| -> mlua::Result<Vec<LevelBonus>> {
        sequence(&table_at(table, key)?, key)?.iter().map(bonus).collect()
    };
    Ok(Occupancy {
        base: number_at(table, "base")?,
        min: number_at(table, "min")?,
        max: number_at(table, "max")?,
        capital: number_at(table, "capital")?,
        military: bonuses("military")?,
        industrial: bonuses("industrial")?,
        strategic: bonuses("strategic")?,
    })
}

fn definitions(value: Value) -> mlua::Result<Definitions> {
    let Value::Table(result) = value else { return Err(mlua::Error::runtime("indices Compile must return a table")); };
    keys(&result, &["attributes", "military", "industrial", "strategic_seconds", "decay", "occupancy"])?;
    let attributes = table_at(&result, "attributes")?;
    keys(&attributes, &["military", "industrial", "strategic"])?;
    let (military, military_points_per_death) = bands(&table_at(&result, "military")?, "military")?;
    let (industrial, industrial_points_per_m3) = bands(&table_at(&result, "industrial")?, "industrial")?;
    let strategic = table_at(&result, "strategic_seconds")?;
    let mut strategic_seconds = Vec::new();
    for pair in strategic.pairs::<Value, Value>() {
        let (Value::Integer(index), Value::Integer(seconds)) = pair? else {
            return Err(mlua::Error::runtime("invalid strategic thresholds"));
        };
        if index < 1 { return Err(mlua::Error::runtime("invalid strategic thresholds")); }
        strategic_seconds.push((index as usize, seconds));
    }
    strategic_seconds.sort_by_key(|(index, _)| *index);
    if strategic_seconds.iter().enumerate().any(|(position, (index, _))| *index != position + 1) {
        return Err(mlua::Error::runtime("strategic thresholds are not a sequence"));
    }
    let strategic_seconds: Vec<i64> = strategic_seconds.into_iter().map(|(_, seconds)| seconds).collect();
    let decay = table_at(&result, "decay")?;
    keys(&decay, &["points", "interval_ticks"])?;
    Ok(Definitions {
        military_attribute: integer_at(&attributes, "military")?,
        industrial_attribute: integer_at(&attributes, "industrial")?,
        strategic_attribute: integer_at(&attributes, "strategic")?,
        military, industrial, strategic_seconds,
        military_points_per_death, industrial_points_per_m3,
        decay_points: number_at(&decay, "points")?,
        decay_interval_ticks: integer_at(&decay, "interval_ticks")?,
        occupancy: occupancy(&table_at(&result, "occupancy")?)?,
    })
}

/// Finite, positive, contiguous and monotone invariants of the compiled tables;
/// no missing or non-finite value is silently repaired. Attribute identifiers are
/// checked against the SDE at activation; retained packages keep their references.
pub(super) fn validate(definitions: &Definitions, sde: Option<&crate::sde::Sde>) -> mlua::Result<()> {
    let unique = [definitions.military_attribute, definitions.industrial_attribute, definitions.strategic_attribute];
    if unique.iter().any(|id| *id <= 0) || (1..unique.len()).any(|i| unique[..i].contains(&unique[i])) {
        return Err(mlua::Error::runtime("invalid indices attribute identifiers"));
    }
    if sde.is_some_and(|sde| unique.iter().any(|id| i32::try_from(*id).ok().and_then(|id| sde.dogma.attribute(id)).is_none())) {
        return Err(mlua::Error::runtime("unknown indices attribute identifier"));
    }
    for bands in [&definitions.military, &definitions.industrial] {
        if bands.len() != LEVEL_BANDS
            || bands.iter().enumerate().any(|(i, band)| band.level != i as i64
                || !band.min_points.is_finite() || !band.max_points.is_finite() || !band.modifier.is_finite()
                || band.min_points < 0.0 || band.max_points <= band.min_points || band.modifier <= 0.0)
            || bands[0].min_points != 0.0
            || bands.windows(2).any(|pair| pair[1].min_points != pair[0].max_points) {
            return Err(mlua::Error::runtime("invalid indices level bands"));
        }
    }
    if definitions.strategic_seconds.len() != STRATEGIC_THRESHOLDS
        || definitions.strategic_seconds.iter().any(|seconds| *seconds <= 0)
        || definitions.strategic_seconds.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(mlua::Error::runtime("invalid strategic thresholds"));
    }
    if !definitions.military_points_per_death.is_finite() || definitions.military_points_per_death <= 0.0
        || !definitions.industrial_points_per_m3.is_finite() || definitions.industrial_points_per_m3 <= 0.0
        || !definitions.decay_points.is_finite() || definitions.decay_points <= 0.0
        || definitions.decay_interval_ticks <= 0 {
        return Err(mlua::Error::runtime("invalid indices point parameters"));
    }
    let occupancy = &definitions.occupancy;
    let finite = [occupancy.base, occupancy.min, occupancy.max, occupancy.capital].iter().all(|v| v.is_finite());
    if !finite || occupancy.min <= 0.0 || occupancy.max < occupancy.min || occupancy.base <= 0.0 || occupancy.capital < 0.0
        || [&occupancy.military, &occupancy.industrial, &occupancy.strategic].iter().any(|bonuses| {
            bonuses.iter().any(|b| !b.bonus.is_finite() || b.bonus < 0.0)
                || (1..bonuses.len()).any(|i| bonuses[..i].iter().any(|b| b.level == bonuses[i].level))
        }) {
        return Err(mlua::Error::runtime("invalid occupancy parameters"));
    }
    Ok(())
}

/// Runs the authored closed `Compile`; the Lua source decides the policy, Rust only
/// validates the typed result. `sde` enables attribute-reference validation.
pub(super) fn compile(lua: &Lua, source: &[u8], authored: &[u8], dataset: &[u8], config: &LuaConfig, sde: Option<&crate::sde::Sde>) -> mlua::Result<Definitions> {
    let authored = self::authored(authored)?;
    let dataset = self::dataset(dataset)?;
    let input = lua.create_table()?;
    let scalars = lua.create_table()?;
    for (key, value) in [
        ("military_points_per_death", authored.military_points_per_death),
        ("industrial_points_per_m3", authored.industrial_points_per_m3),
        ("decay_points", authored.decay_points),
    ] { scalars.set(key, value)?; }
    scalars.set("decay_interval_seconds", authored.decay_interval_seconds)?;
    input.set("authored", scalars)?;
    input.set("dataset", super::super::json::decode(lua, dataset, "dataset", 0)?)?;
    input.set("ticks_per_second", FILETIME_SECOND)?;
    let source = std::str::from_utf8(source).map_err(mlua::Error::external)?;
    let wrapper = format!(r#"local input = ...
        local exports = (function()
{source}
        end)()
        if type(exports) ~= 'table' or type(exports.Compile) ~= 'function' then error('invalid indices exports') end
        for key in pairs(exports) do if key ~= 'Compile' then error('unknown indices export') end end
        return exports.Compile(input)
    "#);
    let definitions = definitions(policy::run_closed(lua, &wrapper, config, (input,), None)?)?;
    validate(&definitions, sde)?;
    Ok(definitions)
}

/// Serialized retained payload: authored JSON, the used dataset bytes and the
/// compiled definitions snapshot.
pub(super) fn retained_data(authored: &[u8], dataset: &[u8], compiled: &Definitions) -> mlua::Result<Vec<u8>> {
    serde_json::to_vec(&IndicesDataRef { authored, dataset, compiled }).map_err(mlua::Error::external)
}

/// Rebuilds a retained package, recompiling the kept authored and dataset bytes and
/// checking the stored compiled snapshot and version/dataset digest identity.
/// `sde` enables the same activation-time reference validation as `compile`.
pub(super) fn rebuild(lua: &Lua, source: &[u8], data: &[u8], version: &str, config: &LuaConfig, sde: Option<&crate::sde::Sde>) -> mlua::Result<Definitions> {
    let retained: IndicesData = serde_json::from_slice(data).map_err(mlua::Error::external)?;
    let maximum = config.data_file_limit_bytes.checked_mul(INDICES_SIZE_FACTOR).ok_or_else(|| mlua::Error::runtime("invalid content limit"))?;
    if retained.authored.len() > config.data_file_limit_bytes || retained.dataset.len() > maximum {
        return Err(mlua::Error::runtime("retained indices package exceeds size limit"));
    }
    if version != format!("{}/{}", self::authored_version(&retained.authored)?, sha256_hex(&retained.dataset)) {
        return Err(mlua::Error::runtime("indices version digest mismatch"));
    }
    let compiled = compile(lua, source, &retained.authored, &retained.dataset, config, sde)?;
    if compiled != retained.compiled {
        return Err(mlua::Error::runtime("retained indices definitions do not match inputs"));
    }
    Ok(compiled)
}

#[cfg(all(test, feature = "tests-lua"))]
mod tests;
