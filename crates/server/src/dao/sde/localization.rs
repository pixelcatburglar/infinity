//! Startup projection of referenced native localization text.
//! This server-startup DAO has no direct client mirror.
//! Sources: `sde_native.localization_fsd_en_us`, `localization_fsd_zh`, `dungeons`, `types`, `marketgroups` and `contributionmethods`.
//! Set-based contribution-ID membership avoids correlated per-localization-row scans.
//! Other languages and unreferenced localization entries are unsupported.

use crate::{db::{executor::Executor, Error}, sde::localization::LocalizedText};

impl LocalizedText {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { id: row.get("id"), text: row.get("text") }
    }
}

pub(crate) async fn english(ex: &(impl Executor + ?Sized)) -> Result<Vec<LocalizedText>, Error> {
    Ok(ex.query("SELECT l.key::bigint AS id, l.data->>'text' AS text FROM sde_native.localization_fsd_en_us l WHERE EXISTS (SELECT 1 FROM sde_native.dungeons d WHERE d.data->>'dungeonNameID' = l.key) OR l.key IN (SELECT c.data->>'titleID' FROM sde_native.contributionmethods c UNION ALL SELECT c.data->>'descriptionID' FROM sde_native.contributionmethods c) ORDER BY l.key::bigint", &[]).await?.iter().map(LocalizedText::from_row).collect())
}

pub(crate) async fn chinese(ex: &(impl Executor + ?Sized)) -> Result<Vec<LocalizedText>, Error> {
    Ok(ex.query("SELECT l.key::bigint AS id, l.data->>'text' AS text FROM sde_native.localization_fsd_zh l WHERE EXISTS (SELECT 1 FROM sde_native.dungeons d WHERE d.data->>'dungeonNameID' = l.key) OR EXISTS (SELECT 1 FROM sde_native.types t WHERE t.data->>'typeNameID' = l.key) OR EXISTS (SELECT 1 FROM sde_native.marketgroups m WHERE m.data->>'nameID' = l.key) OR l.key IN (SELECT c.data->>'titleID' FROM sde_native.contributionmethods c UNION ALL SELECT c.data->>'descriptionID' FROM sde_native.contributionmethods c) ORDER BY l.key::bigint", &[]).await?.iter().map(LocalizedText::from_row).collect())
}
