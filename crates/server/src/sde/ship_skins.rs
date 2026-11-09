//! Immutable first-party SKIN definitions from native static data.
//! `shipskins/static.py` defines regional visibility, hulls and license fields.
//! Runtime ownership, structures and single-use activation are not handled here.
use crate::{dao::sde::ship_skins as dao, db::{self, executor::Executor}};

/// Serenity region identifier from `shipskins/static.py:160`.
const SERENITY_REGION: &str = "optic";
/// Permanent license duration in `panelShipAvailableSkinLicenses.py`.
const PERMANENT_SKIN_DURATION: i32 = -1;

pub(crate) struct Skin {
    pub skin_id: i32,
    pub type_ids: Vec<i32>,
    pub is_structure_skin: bool,
    pub visible_serenity: bool,
    pub visible_tranquility: bool,
}

pub(crate) struct License {
    pub license_type_id: i32,
    pub skin_id: i32,
    pub duration_days: i32,
    pub is_single_use: bool,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct ShipSkins {
    skins: Vec<Skin>,
    licenses: Vec<License>,
}

impl Skin {
    pub(crate) fn supports(&self, type_id: i32) -> bool {
        self.type_ids.contains(&type_id)
    }

    pub(crate) fn visible(&self, region: &str) -> bool {
        if region == SERENITY_REGION { self.visible_serenity } else { self.visible_tranquility }
    }
}

impl License {
    pub(crate) fn is_permanent(&self) -> bool {
        self.duration_days == PERMANENT_SKIN_DURATION
    }
}

impl ShipSkins {
    pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Self, db::Error> {
        let (skins, licenses) = tokio::try_join!(dao::skins(ex), dao::licenses(ex))?;
        Ok(Self { skins, licenses })
    }

    pub(crate) fn skin(&self, id: i32) -> Option<&Skin> {
        self.skins.binary_search_by_key(&id, |skin| skin.skin_id).ok().map(|i| &self.skins[i])
    }

    pub(crate) fn license(&self, type_id: i32) -> Option<&License> {
        self.licenses.iter().find(|license| license.license_type_id == type_id)
    }

    pub(crate) fn licenses(&self) -> impl Iterator<Item = &License> {
        self.licenses.iter()
    }
}

#[cfg(test)] mod tests;
