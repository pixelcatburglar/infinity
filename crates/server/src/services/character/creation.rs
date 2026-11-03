//! Character creation and name validation, shared independently of client connections.
//! The client calls charUnboundMgr.CreateCharacterWithDoll from
//! eve/client/script/ui/services/ccSvc.py:92-97, reached from characterCreationContainer.py:1840.
//! Reads SDE school and race starting data and atomically writes characters and starting skills.
//! An optional explicit station is validated against the SDE; client creation selects a school station.
//! Ship provisioning is deferred to selection; appearance and portrait are opaque bytes.
use crate::{dao::characters, db::Db, messages, sde::Sde};
use crate::services::{db_err, CallError};
use anyhow::Context;
use std::sync::Arc;

/// Client chooseNameSection.py valid-name result.
pub(crate) const NAME_VALID: i64 = 1;
/// Client chooseNameSection.py short-name result.
const NAME_TOO_SHORT: i64 = -1;
/// Client chooseNameSection.py long-name result.
const NAME_TOO_LONG: i64 = -2;
/// Client chooseNameSection.py illegal-character result.
const NAME_ILLEGAL_CHARACTER: i64 = -5;
/// Client chooseNameSection.py too-many-spaces result.
const NAME_TOO_MANY_SPACES: i64 = -6;
/// Client chooseNameSection.py consecutive-spaces result.
const NAME_CONSECUTIVE_SPACES: i64 = -7;
/// Client chooseNameSection.py unavailable-name result.
const NAME_UNAVAILABLE: i64 = -101;

pub(crate) struct NewPilot {
    pub name: String,
    pub race: i64,
    pub bloodline: i64,
    pub ancestry: Option<i64>,
    pub gender: i64,
    pub school: Option<i64>,
    pub station: Option<i64>,
    pub balance: f64,
    pub appearance: Vec<u8>,
    pub portrait: Vec<u8>,
}

pub(crate) struct CreatedPilot {
    pub char_id: i64,
    pub station_id: i64,
}

fn name_shape_code(name: &str) -> i64 {
    let policy = &crate::config::game().characters;
    let n = name.chars().count() as u128;
    if n < policy.name_min_len as u128 {
        return NAME_TOO_SHORT;
    }
    if n > policy.name_max_len as u128 {
        return NAME_TOO_LONG;
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '\'') {
        return NAME_ILLEGAL_CHARACTER;
    }
    if name.contains("  ") {
        return NAME_CONSECUTIVE_SPACES;
    }
    if name.matches(' ').count() as u128 > policy.name_max_spaces as u128 {
        return NAME_TOO_MANY_SPACES;
    }
    NAME_VALID
}

pub(crate) async fn name_code(db: &Db, name: &str) -> Result<i64, CallError> {
    let code = name_shape_code(name);
    if code != NAME_VALID {
        return Ok(code);
    }
    let taken = characters::creation::name_taken(db, name).await.map_err(db_err)?;
    Ok(if taken { NAME_UNAVAILABLE } else { NAME_VALID })
}

/// (school corporation, station, solar system, constellation, region) for a new character.
fn starting_location(sde: &Sde, school_id: Option<i64>, race_id: i64, station_id: Option<i64>) -> Result<(i64, i64, i64, i64, i64), CallError> {
    let school = match school_id {
        Some(id) => sde.characters.school(id),
        None => sde.characters.schools_for_race(race_id as i32).next(),
    }.context("no starting station for school/race")?;
    let station = if let Some(id) = station_id {
        sde.map.station(id).context("unknown starting station")?
    } else {
        let stations = || school.starting_stations.iter().filter_map(|&id| sde.map.station(id));
        let count = stations().count();
        stations().nth((crate::services::rand01() * count as f64) as usize).context("no starting station for school/race")?
    };
    Ok((
        school.corporation_id.context("school has no corporation")?,
        station.station_id,
        station.solar_system_id.context("starting station has no solar system")?,
        station.constellation_id.context("starting station has no constellation")?,
        station.region_id.context("starting station has no region")?,
    ))
}

pub(crate) async fn create(db: &Db, sde: &Arc<Sde>, user_id: i64, pilot: NewPilot) -> Result<CreatedPilot, CallError> {
    if name_code(db, &pilot.name).await? != NAME_VALID {
        return Err(CallError::User { msg: messages::MSG_CHAR_NAME_INVALID.into(), dict: vec![] });
    }
    let (corp, station, system, constellation, region) = starting_location(sde, pilot.school, pilot.race, pilot.station)?;
    let pilot = Arc::new(pilot);
    let created = db.transaction(
        |tx, _: &mut ()| {
            let pilot = Arc::clone(&pilot);
            let sde = Arc::clone(sde);
            Box::pin(async move {
                let char_id = characters::creation::create(tx, &characters::creation::NewCharacter {
                    user_id, name: &pilot.name, race_id: pilot.race as i32, bloodline_id: pilot.bloodline as i32,
                    gender_id: pilot.gender as i32, ancestry_id: pilot.ancestry.map(|v| v as i32), school_id: pilot.school.map(|v| v as i32),
                    corporation_id: corp, station_id: station, solar_system_id: system, constellation_id: constellation, region_id: region,
                    appearance: &pilot.appearance, portrait: &pilot.portrait, balance: pilot.balance,
                }).await.map_err(db_err)?;
                crate::services::skills::starting::grant_starting_skills(tx, &sde, char_id, pilot.race as i32).await?;
                Ok::<_, CallError>(CreatedPilot { char_id, station_id: station })
            })
        },
        |value, ()| async move { Ok(value) },
    ).await?;
    let name = &pilot.name;
    let char_id = created.char_id;
    let station = created.station_id;
    tracing::info!(user = user_id, "created character {name:?} ({char_id}) in station {station}");
    Ok(created)
}

#[cfg(test)]
mod tests;
