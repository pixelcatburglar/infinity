//! Startup repairable-ship FSD configuration and variation projections from
//! `sde_native.repairabletypesconfig`, `sde_native.typerefurbishedcomponentconfig`,
//! `sde_native.unrepairableitemsconfig`, `sde_native.typematerials`, native
//! `sde_native.types` JSON for variation parent and meta fields, and typed
//! `sde.inv_types` for catalog identifiers, groups, categories and publication.
//! Consumers mirror `repairableShip/fsd` and `repairableShip/common/methods.py`;
//! validation belongs to `sde::repairable_ship`.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

/// One whole single-row native configuration document as JSON text.
#[derive(Clone)]
pub(crate) struct ConfigRow {
    pub(crate) data: String,
}
impl ConfigRow {
    fn from_row(row: &Row) -> Self {
        Self { data: row.get("data") }
    }
}

/// `repairableShip/fsd` RepairableTypesConfigFSDLoader.
pub(crate) async fn repairable_types(ex: &(impl Executor + ?Sized)) -> Result<Vec<ConfigRow>, db::Error> {
    Ok(ex.query("SELECT data::text AS data FROM sde_native.repairabletypesconfig ORDER BY row_no", &[]).await?
        .iter().map(ConfigRow::from_row).collect())
}

/// `repairableShip/fsd` TypeRefurbishedComponentConfigFSDLoader.
pub(crate) async fn refurbished_components(ex: &(impl Executor + ?Sized)) -> Result<Vec<ConfigRow>, db::Error> {
    Ok(ex.query("SELECT data::text AS data FROM sde_native.typerefurbishedcomponentconfig ORDER BY row_no", &[]).await?
        .iter().map(ConfigRow::from_row).collect())
}

/// `repairableShip/fsd` UnRepairableItemsConfigFSDLoader.
pub(crate) async fn unrepairable_items(ex: &(impl Executor + ?Sized)) -> Result<Vec<ConfigRow>, db::Error> {
    Ok(ex.query("SELECT data::text AS data FROM sde_native.unrepairableitemsconfig ORDER BY row_no", &[]).await?
        .iter().map(ConfigRow::from_row).collect())
}

/// Per-type variation facts: `sde.inv_types` supplies catalog identifiers,
/// groups, categories and publication; `sde_native.types` JSON supplies parent
/// and meta fields. A parent is projected only for published types, mirroring
/// `evetypes.GetVariationParentTypeIDOrNone` reachability.
#[derive(Clone)]
pub(crate) struct VariationRow {
    pub(crate) type_id: i32,
    pub(crate) group_id: Option<i32>,
    pub(crate) category_id: Option<i32>,
    pub(crate) parent: Option<i32>,
    pub(crate) meta_level: Option<f64>,
}
impl VariationRow {
    fn from_row(row: &Row) -> Self {
        Self {
            type_id: row.get("type_id"),
            group_id: row.get("group_id"),
            category_id: row.get("category_id"),
            parent: row.get("parent"),
            meta_level: row.get("meta_level"),
        }
    }
}
pub(crate) async fn variations(ex: &(impl Executor + ?Sized)) -> Result<Vec<VariationRow>, db::Error> {
    Ok(ex.query(
        "SELECT t.type_id AS type_id, t.group_id AS group_id, t.category_id AS category_id, \
         CASE WHEN t.published THEN (n.data->>'variationParentTypeID')::integer END AS parent, \
         (n.data->>'metaLevel')::double precision AS meta_level \
         FROM sde.inv_types t LEFT JOIN sde_native.types n ON n.key::integer = t.type_id \
         ORDER BY t.type_id",
        &[],
    ).await?.iter().map(VariationRow::from_row).collect())
}

/// Types whose `typeMaterials.randomizedMaterials` list is non-empty; presence
/// only, the randomized drops themselves are not projected.
#[derive(Clone)]
pub(crate) struct RandomizedMaterialRow {
    pub(crate) type_id: i32,
}
impl RandomizedMaterialRow {
    fn from_row(row: &Row) -> Self {
        Self { type_id: row.get("type_id") }
    }
}
pub(crate) async fn randomized_materials(ex: &(impl Executor + ?Sized)) -> Result<Vec<RandomizedMaterialRow>, db::Error> {
    Ok(ex.query(
        "SELECT key::integer AS type_id FROM sde_native.typematerials \
         WHERE jsonb_typeof(data->'randomizedMaterials') = 'array' \
         AND jsonb_array_length(data->'randomizedMaterials') > 0 ORDER BY key::integer",
        &[],
    ).await?.iter().map(RandomizedMaterialRow::from_row).collect())
}

/// Every known group or category ID for unrepairable-reference validation.
#[derive(Clone)]
pub(crate) struct CatalogueRow {
    pub(crate) group_id: Option<i32>,
    pub(crate) category_id: Option<i32>,
}
impl CatalogueRow {
    fn from_row(row: &Row) -> Self {
        Self { group_id: row.get("group_id"), category_id: row.get("category_id") }
    }
}
pub(crate) async fn catalogue(ex: &(impl Executor + ?Sized)) -> Result<Vec<CatalogueRow>, db::Error> {
    Ok(ex.query(
        "SELECT group_id, NULL::integer AS category_id FROM sde.inv_groups \
         UNION ALL SELECT NULL::integer, category_id FROM sde.inv_categories",
        &[],
    ).await?.iter().map(CatalogueRow::from_row).collect())
}
