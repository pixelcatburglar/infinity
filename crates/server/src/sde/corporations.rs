//! Immutable NPC corporation records shared by gameplay features.
//! This server-startup dataset has no direct client mirror.
//! Source: `sde.crp_npc_corporations`. Dynamic member counts and other fields are unsupported.

use crate::{dao, db::Db};

pub(crate) struct NpcCorporation {
    pub corporation_id: i64,
    pub name: Option<String>,
    pub ticker: Option<String>,
    pub lower_name: Option<String>,
    pub upper_ticker: Option<String>,
    pub description: Option<String>,
    pub tax_rate: Option<f64>,
    pub ceo_id: Option<i64>,
    pub station_id: Option<i64>,
    pub race_id: Option<i64>,
    pub faction_id: Option<i64>,
    pub member_limit: Option<i64>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct NpcCorporations {
    rows: Vec<NpcCorporation>,
}

impl NpcCorporations {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Ok(Self { rows: dao::sde::corporations::load(db).await? })
    }

    pub(crate) fn get(&self, id: i64) -> Option<&NpcCorporation> {
        self.rows.binary_search_by_key(&id, |row| row.corporation_id).ok().map(|index| &self.rows[index])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &NpcCorporation> { self.rows.iter() }
    pub(crate) fn len(&self) -> usize { self.rows.len() }
}
