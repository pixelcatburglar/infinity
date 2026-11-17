//! Startup dogma metadata and ordered modifier projections from typed `sde` tables.
//! This DAO has no direct client mirror; fitting consumers include
//! `dogma/effects/modifiers.py:8-140` and `dogma/const.py:7-51`.
//! Full modifier JSON and unsupported effect fields are not retained.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct AttributeRow {
    pub(crate) attribute_id: i32,
    pub(crate) name: Option<String>,
    pub(crate) default_value: Option<f64>,
    pub(crate) stackable: Option<bool>,
}
impl AttributeRow {
    fn from_row(row: &Row) -> Self {
        Self {
            attribute_id: row.get("attribute_id"),
            name: row.get("name"),
            default_value: row.get("default_value"),
            stackable: row.get("stackable"),
        }
    }
}
pub(crate) async fn attributes(ex: &(impl Executor + ?Sized)) -> Result<Vec<AttributeRow>, db::Error> {
    Ok(ex.query("SELECT attribute_id, name, default_value::float8 AS default_value, stackable FROM sde.dogma_attributes ORDER BY attribute_id", &[]).await?.iter().map(AttributeRow::from_row).collect())
}
pub(crate) struct EffectRow {
    pub(crate) effect_id: i32,
    pub(crate) name: Option<String>,
    pub(crate) guid: Option<String>,
    pub(crate) effect_category: Option<i32>,
    pub(crate) duration_attribute_id: Option<i32>,
    pub(crate) fitting_usage_chance_attribute_id: Option<i32>,
    pub(crate) is_offensive: bool, pub(crate) is_assistance: bool,
    pub(crate) disallow_auto_repeat: bool, pub(crate) electronic_chance: bool, pub(crate) propulsion_chance: bool,
    pub(crate) discharge_attribute_id: Option<i32>, pub(crate) range_attribute_id: Option<i32>,
    pub(crate) falloff_attribute_id: Option<i32>, pub(crate) tracking_attribute_id: Option<i32>,
    pub(crate) resistance_attribute_id: Option<i32>,
}
impl EffectRow {
    fn from_row(row: &Row) -> Self {
        Self {
            effect_id: row.get("effect_id"),
            name: row.get("name"),
            guid: row.get("guid"),
            effect_category: row.get("effect_category"),
            duration_attribute_id: row.get("duration_attribute_id"),
            fitting_usage_chance_attribute_id: row.get("fitting_usage_chance_attribute_id"),
            is_offensive: row.get("is_offensive"), is_assistance: row.get("is_assistance"),
            disallow_auto_repeat: row.get("disallow_auto_repeat"), electronic_chance: row.get("electronic_chance"),
            propulsion_chance: row.get("propulsion_chance"), discharge_attribute_id: row.get("discharge_attribute_id"),
            range_attribute_id: row.get("range_attribute_id"), falloff_attribute_id: row.get("falloff_attribute_id"),
            tracking_attribute_id: row.get("tracking_attribute_id"),
            resistance_attribute_id: row.get("resistance_attribute_id"),
        }
    }
}
pub(crate) async fn effects(ex: &(impl Executor + ?Sized)) -> Result<Vec<EffectRow>, db::Error> {
    Ok(ex.query("SELECT effect_id, name, guid, effect_category, duration_attribute_id, fitting_usage_chance_attribute_id, COALESCE(is_offensive, false) AS is_offensive, COALESCE(is_assistance, false) AS is_assistance, COALESCE(disallow_auto_repeat, false) AS disallow_auto_repeat, COALESCE(electronic_chance, false) AS electronic_chance, COALESCE(propulsion_chance, false) AS propulsion_chance, discharge_attribute_id, range_attribute_id, falloff_attribute_id, tracking_speed_attribute_id AS tracking_attribute_id, resistance_attribute_id FROM sde.dogma_effects ORDER BY effect_id", &[]).await?.iter().map(EffectRow::from_row).collect())
}
/// Server sentinel for an absent modifier operation, outside supported operations.
const MISSING_MODIFIER_OPERATION: i32 = -100;
pub(crate) struct ModifierRow {
    pub(crate) effect_id: i32, pub(crate) func: String, pub(crate) domain: String,
    pub(crate) op: i32, pub(crate) modified: i32, pub(crate) modifying: i32,
    pub(crate) group: Option<i32>, pub(crate) skill: Option<i32>,
}
impl ModifierRow {
    fn from_row(row: &Row) -> Self {
        Self { effect_id: row.get("effect_id"), func: row.get("func"),
            domain: row.get("domain"), op: row.get("op"),
            modified: row.get("modified"), modifying: row.get("modifying"),
            group: row.get("group"), skill: row.get("skill") }
    }
}
pub(crate) async fn modifiers(ex: &(impl Executor + ?Sized)) -> Result<Vec<ModifierRow>, db::Error> {
    Ok(ex.query("SELECT e.effect_id, COALESCE(m.value->>'func', '') AS func, COALESCE(m.value->>'domain', '') AS domain, COALESCE((m.value->>'operation')::int, $1) AS op, COALESCE((m.value->>'modifiedAttributeID')::int, 0) AS modified, COALESCE((m.value->>'modifyingAttributeID')::int, 0) AS modifying, (m.value->>'groupID')::int AS \"group\", (m.value->>'skillTypeID')::int AS skill FROM sde.dogma_effects e CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(e.modifier_info) = 'array' THEN e.modifier_info ELSE '[]'::jsonb END) WITH ORDINALITY AS m(value, ordinal) ORDER BY e.effect_id, m.ordinal", &[&MISSING_MODIFIER_OPERATION]).await?.iter().map(ModifierRow::from_row).collect())
}
