//! Startup NPC station graphic-location projection from native types, graphic IDs and locations.
//! `ballparkCommon/docking.py:GetUndockDirections` follows the same links.
//! Missing links have no locators; no runtime SQL is used.
use crate::db::{executor::Executor, Error};

pub(crate) struct GraphicLocationRow {
    pub(crate) type_id: i32,
    pub(crate) directional_locators: String,
}

impl GraphicLocationRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { type_id: row.get("type_id"), directional_locators: row.get("directional_locators") }
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<GraphicLocationRow>, Error> {
    Ok(ex.query("SELECT t.key::int AS type_id, COALESCE(l.data->'directionalLocators', '[]'::jsonb)::text AS directional_locators
        FROM sde_native.types t
        JOIN sde_native.graphicids g ON g.key = t.data->>'graphicID'
        JOIN sde_native.graphiclocations l ON l.key = g.data->>'graphicLocationID'
        WHERE EXISTS (SELECT 1 FROM sde.sta_stations s WHERE s.type_id = t.key::int)", &[])
        .await?.iter().map(GraphicLocationRow::from_row).collect())
}
