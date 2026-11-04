//! Startup projections of native SKINs and licenses for `shipskins/static.py`.
//! Only fields consumed by the server are loaded; ownership is dynamic game data.
use crate::{db::{executor::Executor, Error}, sde::ship_skins::{License, Skin}};

impl Skin {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            skin_id: row.get("skin_id"),
            type_ids: row.get("type_ids"),
            is_structure_skin: row.get("is_structure_skin"),
            visible_serenity: row.get("visible_serenity"),
            visible_tranquility: row.get("visible_tranquility"),
        }
    }
}

impl License {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            license_type_id: row.get("license_type_id"),
            skin_id: row.get("skin_id"),
            duration_days: row.get("duration_days"),
            is_single_use: row.get("is_single_use"),
        }
    }
}

pub(crate) async fn skins(ex: &(impl Executor + ?Sized)) -> Result<Vec<Skin>, Error> {
    Ok(ex.query("SELECT key::int AS skin_id, ARRAY(SELECT value::int FROM jsonb_array_elements_text(COALESCE(data->'types', '[]'::jsonb)) WITH ORDINALITY AS t(value, ord) ORDER BY ord) AS type_ids, COALESCE((data->>'isStructureSkin')::boolean, false) AS is_structure_skin, COALESCE((data->>'visibleSerenity')::boolean, false) AS visible_serenity, COALESCE((data->>'visibleTranquility')::boolean, false) AS visible_tranquility FROM sde_native.skins ORDER BY key::int", &[]).await?.iter().map(Skin::from_row).collect())
}

pub(crate) async fn licenses(ex: &(impl Executor + ?Sized)) -> Result<Vec<License>, Error> {
    Ok(ex.query("SELECT key::int AS license_type_id, (data->>'skinID')::int AS skin_id, (data->>'duration')::int AS duration_days, COALESCE((data->>'isSingleUse')::boolean, false) AS is_single_use FROM sde_native.skinlicenses ORDER BY key::int", &[]).await?.iter().map(License::from_row).collect())
}
