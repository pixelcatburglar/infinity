//! Immutable faction records shared by gameplay features.
//! This server-startup dataset has no direct client mirror.
//! Source: `sde.chr_factions`, including home solar systems for the warzone dashboard.
//! Dynamic faction state and other faction fields are unsupported.

use crate::{dao, db::Db};

pub(crate) struct Faction {
    pub faction_id: i64,
    pub name: Option<String>,
    pub lower_name: Option<String>,
    pub corporation_id: Option<i64>,
    pub solar_system_id: Option<i64>,
}

pub(crate) struct Factions {
    rows: Vec<Faction>,
}

impl Factions {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Ok(Self { rows: dao::sde::factions::load(db).await? })
    }

    pub(crate) fn get(&self, id: i64) -> Option<&Faction> {
        self.rows.binary_search_by_key(&id, |row| row.faction_id).ok().map(|index| &self.rows[index])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Faction> { self.rows.iter() }
    pub(crate) fn len(&self) -> usize { self.rows.len() }
}
