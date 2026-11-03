//! Startup inventory and dogma association projections from typed `sde` tables.
//! This DAO has no direct client mirror; an attribute consumer is
//! `eve/common/script/dogma/baseDogmaLocation.py:1960-1967`.
//! Only consumed columns are loaded, not raw JSON.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct TypeRow {
    pub(crate) type_id: i32,
    pub(crate) group_id: Option<i32>,
    pub(crate) category_id: Option<i32>,
    pub(crate) name: Option<String>,
    pub(crate) lower_name: Option<String>,
    pub(crate) mass: Option<f64>,
    pub(crate) capacity: Option<f64>,
    pub(crate) volume: Option<f64>,
    pub(crate) radius: Option<f64>,
    pub(crate) portion_size: Option<i32>,
    pub(crate) base_price: Option<f64>,
    pub(crate) published: Option<bool>,
    pub(crate) market_group_id: Option<i32>,
    pub(crate) race_id: Option<i32>,
    pub(crate) icon_id: Option<i32>,
}
impl TypeRow {
    fn from_row(row: &Row) -> Self {
        Self {
            type_id: row.get("type_id"),
            group_id: row.get("group_id"),
            category_id: row.get("category_id"),
            name: row.get("name"),
            lower_name: row.get("lower_name"),
            mass: row.get("mass"),
            capacity: row.get("capacity"),
            volume: row.get("volume"),
            radius: row.get("radius"),
            portion_size: row.get("portion_size"),
            base_price: row.get("base_price"),
            published: row.get("published"),
            market_group_id: row.get("market_group_id"),
            race_id: row.get("race_id"),
            icon_id: row.get("icon_id"),
        }
    }
}
pub(crate) async fn types(ex: &(impl Executor + ?Sized)) -> Result<Vec<TypeRow>, db::Error> {
    Ok(ex.query("SELECT type_id, group_id, category_id, name, lower(name) AS lower_name, mass::float8 AS mass, capacity::float8 AS capacity, volume::float8 AS volume, radius::float8 AS radius, portion_size, base_price::float8 AS base_price, published, market_group_id, race_id, icon_id FROM sde.inv_types ORDER BY type_id", &[]).await?.iter().map(TypeRow::from_row).collect())
}
pub(crate) struct GroupRow {
    pub(crate) group_id: i32,
    pub(crate) category_id: Option<i32>,
    pub(crate) name: Option<String>,
    pub(crate) lower_name: Option<String>,
}
impl GroupRow {
    fn from_row(row: &Row) -> Self {
        Self {
            group_id: row.get("group_id"),
            category_id: row.get("category_id"),
            name: row.get("name"),
            lower_name: row.get("lower_name"),
        }
    }
}
pub(crate) async fn groups(ex: &(impl Executor + ?Sized)) -> Result<Vec<GroupRow>, db::Error> {
    Ok(ex.query("SELECT group_id, category_id, name, lower(name) AS lower_name FROM sde.inv_groups ORDER BY group_id", &[]).await?.iter().map(GroupRow::from_row).collect())
}
pub(crate) struct CategoryRow {
    pub(crate) category_id: i32,
    pub(crate) name: Option<String>,
}
impl CategoryRow {
    fn from_row(row: &Row) -> Self {
        Self {
            category_id: row.get("category_id"),
            name: row.get("name"),
        }
    }
}
pub(crate) async fn categories(ex: &(impl Executor + ?Sized)) -> Result<Vec<CategoryRow>, db::Error> {
    Ok(ex.query("SELECT category_id, name FROM sde.inv_categories ORDER BY category_id", &[]).await?.iter().map(CategoryRow::from_row).collect())
}
pub(crate) struct AttributeRow {
    pub(crate) type_id: i32,
    pub(crate) attribute_id: i32,
    pub(crate) value: f64,
}
impl AttributeRow {
    fn from_row(row: &Row) -> Self {
        Self {
            type_id: row.get("type_id"),
            attribute_id: row.get("attribute_id"),
            value: row.get("value"),
        }
    }
}
pub(crate) async fn attributes(ex: &(impl Executor + ?Sized)) -> Result<Vec<AttributeRow>, db::Error> {
    Ok(ex.query("SELECT type_id, attribute_id, value::float8 AS value FROM sde.type_attributes ORDER BY type_id, attribute_id", &[]).await?.iter().map(AttributeRow::from_row).collect())
}
pub(crate) struct EffectRow {
    pub(crate) type_id: i32,
    pub(crate) effect_id: i32,
    pub(crate) is_default: bool,
}
impl EffectRow {
    fn from_row(row: &Row) -> Self {
        Self {
            type_id: row.get("type_id"),
            effect_id: row.get("effect_id"),
            is_default: row.get("is_default"),
        }
    }
}
pub(crate) async fn effects(ex: &(impl Executor + ?Sized)) -> Result<Vec<EffectRow>, db::Error> {
    Ok(ex.query("SELECT type_id, effect_id, is_default FROM sde.type_effects ORDER BY type_id, effect_id", &[]).await?.iter().map(EffectRow::from_row).collect())
}
pub(crate) struct MaterialRow {
    pub(crate) type_id: i32,
    pub(crate) material_type_id: i32,
    pub(crate) quantity: Option<i64>,
}
impl MaterialRow {
    fn from_row(row: &Row) -> Self {
        Self {
            type_id: row.get("type_id"),
            material_type_id: row.get("material_type_id"),
            quantity: row.get("quantity"),
        }
    }
}
pub(crate) async fn materials(ex: &(impl Executor + ?Sized)) -> Result<Vec<MaterialRow>, db::Error> {
    Ok(ex.query("SELECT type_id, material_type_id, quantity FROM sde.inv_type_materials ORDER BY type_id, material_type_id", &[]).await?.iter().map(MaterialRow::from_row).collect())
}
