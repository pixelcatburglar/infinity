//! charUnboundMgr: character selection and creation before a character is
//! chosen (loginCharacterselection/charselData.py, eve/client/script/ui/login).
//! Uses character, account, item and skill DAOs and SDE starting data; appearance is stored without interpretation.
//! Slot counts, name limits, starting ISK and deletion delay are operator policies.
//! Selecting a character registers process-local presence; appearance editing is not interpreted here.
//! Selection reconciles the active fit before restoring the ship's persisted condition.
//! Successful selection and logoff hand off lifecycle events after session changes.
//! First selection serializes rookie ship grants on the character row and grants a
//! persistent server-selected civilian fit without charges; existing ships are not retroactively refitted.

mod rookie_fit;

use crate::dao::{accounts, characters, items, skills};
use super::{db_err, keyval, trimmed_arg_text, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::{ACCOUNT_CASH, FAR_FUTURE, FILETIME_SECOND, FILETIME_UNIX_EPOCH, FLAG_HANGAR};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::lua::events::GameEvent;
use crate::messages;
use anyhow::Context;
use marshal::Value;
use tracing::{info, warn};

/// Client chooseNameSection.py valid-name result.
const NAME_VALID: i64 = 1;
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
/// SDE invTypes: Velator, the fallback rookie ship.
const FALLBACK_ROOKIE_SHIP: i32 = 606;
/// SDE chrRaces: Caldari, used when the creation argument is missing.
const FALLBACK_RACE_ID: i64 = 1;
/// SDE chrBloodlines: Deteis, used when the creation argument is missing.
const FALLBACK_BLOODLINE_ID: i64 = 1;
/// Server policy: number of random-name attempts.
const RANDOM_NAME_ATTEMPTS: usize = 50;
/// Server random-name policy: lowest numerical suffix.
const RANDOM_NAME_SUFFIX_START: usize = 100;
/// Server random-name policy: number of possible suffixes.
const RANDOM_NAME_SUFFIX_COUNT: usize = 900;

fn arg_int(call: &Call<'_>, i: usize) -> Option<i64> {
    call.args.get(i).and_then(|v| v.as_int())
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

async fn name_code(c: &Client, name: &str) -> Result<i64, CallError> {
    let code = name_shape_code(name);
    if code != NAME_VALID {
        return Ok(code);
    }
    let taken = characters::creation::name_taken(&c.db, name).await.map_err(db_err)?;
    Ok(if taken { NAME_UNAVAILABLE } else { NAME_VALID })
}

/// (school corporation, station, solar system, constellation, region) for a new character.
async fn starting_location(c: &Client, school_id: Option<i64>, race_id: i64) -> Result<(i64, i64, i64, i64, i64), CallError> {
    let school = match school_id {
        Some(id) => c.sde.characters.school(id),
        None => c.sde.characters.schools_for_race(race_id as i32).next(),
    }.context("no starting station for school/race")?;
    let stations = || school.starting_stations.iter().filter_map(|&id| c.sde.map.station(id));
    let count = stations().count();
    let station = stations().nth((super::rand01() * count as f64) as usize).context("no starting station for school/race")?;
    Ok((
        school.corporation_id.context("school has no corporation")?,
        station.station_id,
        station.solar_system_id.context("starting station has no solar system")?,
        station.constellation_id.context("starting station has no constellation")?,
        station.region_id.context("starting station has no region")?,
    ))
}

async fn character_rows(c: &Client) -> Result<Vec<Value>, CallError> {
    let rows = characters::selection::characters(&c.db, c.login.user_id, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            keyval([
                ("characterID", Value::Int(r.character_id)),
                ("characterName", Value::unicode(r.name.clone())),
                ("corporationID", Value::Int(r.corporation_id)),
                ("allianceID", Value::None),
                ("factionID", Value::None),
                ("stationID", r.station_id.map(Value::Int).unwrap_or(Value::None)),
                ("solarSystemID", r.solar_system_id.map(Value::Int).unwrap_or(Value::None)),
                ("structureID", Value::None),
                ("balance", Value::Float(r.balance)),
                ("skillPoints", Value::Int(r.skill_points)),
                ("logoffDate", Value::Int(r.logoff_date)),
                ("deletePrepareDateTime", r.delete_prepare_at.map(Value::Int).unwrap_or(Value::None)),
                ("genderID", Value::Int(r.gender_id as i64)),
                ("bloodlineID", Value::Int(r.bloodline_id as i64)),
                ("raceID", Value::Int(r.race_id as i64)),
                ("shipTypeID", r.ship_type_id.map(|t| Value::Int(t as i64)).unwrap_or(Value::Int(FALLBACK_ROOKIE_SHIP as i64))),
                ("gender", Value::Int(r.gender_id as i64)),
                ("balanceChange", Value::Float(0.0)),
                ("paperdollState", Value::Int(0)),
                // Skill training summary: nothing in training.
                ("skillTypeID", Value::None),
                ("toLevel", Value::None),
                ("trainingStartTime", Value::None),
                ("trainingEndTime", Value::None),
                ("queueEndTime", Value::None),
                ("finishSP", Value::None),
                ("trainedSP", Value::None),
                ("finishedSkills", Value::list([])),
                ("unreadMailCount", Value::Int(0)),
                ("unprocessedNotifications", Value::Int(0)),
                ("lockTypeID", Value::None),
            ])
        })
        .collect())
}


/// userSvc.UserLogOffCharacter(): back to character selection. The character leaves the
/// game as on a disconnect and the session drops everything about it.
pub async fn log_off(c: &mut Client) -> CallResult {
    let Some(char_id) = c.session.int("charid") else { return Ok(Value::Bool(true)) };
    super::combat::reset(c);
    c.leave_world().await.map_err(CallError::Internal)?;
    characters::selection::logoff(&c.db, char_id).await.map_err(db_err)?;
    let cleared = [
        "charid", "corpid", "corprole", "rolesAtAll", "rolesAtBase", "rolesAtHQ", "rolesAtOther", "corpAccountKey", "allianceid", "warfactionid",
        "genderID", "bloodlineID", "raceID", "hqID", "stationid", "solarsystemid", "solarsystemid2", "constellationid", "regionid", "shipid",
    ];
    c.change_session(cleared.into_iter().map(|k| (k, Value::None)).collect());
    let mut outbox = Outbox::default();
    outbox.event(GameEvent::CharacterLoggedOff { user_id: c.login.user_id, char_id });
    dispatch_client((), outbox, c)?;
    info!(user = c.login.user_id, "character {char_id} logged off to character selection");
    Ok(Value::Bool(true))
}

/// A character of this account that isn't deleted.
async fn own_character(c: &Client, call: &Call<'_>) -> Result<i64, CallError> {
    let char_id = arg_int(call, 0).unwrap_or(0);
    if !characters::selection::owns(&c.db, char_id, c.login.user_id).await.map_err(db_err)? {
        return Err(CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] });
    }
    Ok(char_id)
}

/// Give the character a rookie ship in their station hangar if they have none.
async fn ensure_ship(c: &Client, char_id: i64, race_id: i32, station_id: i64) -> Result<i64, CallError> {
    let ship_type = c.sde.characters.race(race_id as i64).and_then(|race| race.ship_type_id).unwrap_or(FALLBACK_ROOKIE_SHIP);
    let ship_id = c.db.transaction(
        |tx, _: &mut Outbox| Box::pin(async move {
            characters::selection::lock_for_ship_grant(tx, char_id).await?;
            if let Some(ship_id) = characters::selection::ship_id(tx, char_id).await? {
                return Ok::<i64, CallError>(ship_id);
            }
            let fit = rookie_fit::for_ship(ship_type)
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("no rookie fit for ship type {ship_type}")))?;
            let ship_id = items::creation::rookie_ship(tx, fit.ship, char_id, station_id, FLAG_HANGAR).await?;
            items::creation::fitted_items(tx, ship_id, char_id, fit.items).await?;
            characters::selection::set_ship(tx, char_id, ship_id).await?;
            Ok(ship_id)
        }),
        |value, _| async move { Ok(value) },
    ).await?;
    Ok(ship_id)
}

/// Push the docked-in-station session for `char_id` (eveSessions.GetSessionValuesFromRowset).
async fn select_character(c: &mut Client, char_id: i64) -> Result<(), CallError> {
    let r = characters::selection::selected(&c.db, char_id, c.login.user_id).await.map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
    let station = r.station_id;
    let system = r.solar_system_id;
    let race = r.race_id;
    // A character that logged off in space resumes in its ship at the stored position.
    let space_ship = characters::selection::space_ship(&c.db, char_id).await.map_err(db_err)?;
    let ship = match (&space_ship, station) {
        (Some(s), None) => s.item_id,
        (_, Some(st)) => ensure_ship(c, char_id, race, st).await?,
        (None, None) => return Err(CallError::Internal(anyhow::anyhow!("character {char_id} is in space without a ship"))),
    };
    let has_skills = skills::selection::has_skills(&c.db, char_id).await.map_err(db_err)?;
    if !has_skills {
        super::skills::starting::grant_starting_skills(c, char_id, race).await?;
    }
    characters::selection::logoff(&c.db, char_id).await.map_err(db_err)?;
    super::effects::online::reconcile_ship(c, char_id, ship).await?;
    if station.is_some() {
        super::combat::load_docked_condition(&c.db, &c.world, char_id, ship).await?;
    }
    let int = Value::Int;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    let corp = r.corporation_id;
    let roles = r.roles;
    let alliance = r.alliance_id;
    c.change_session(vec![
        ("charid", int(char_id)),
        ("corpid", int(corp)),
        ("allianceid", opt(alliance)),
        ("corprole", int(roles)),
        ("rolesAtAll", int(roles)),
        ("rolesAtBase", int(roles)),
        ("rolesAtHQ", int(roles)),
        ("rolesAtOther", int(roles)),
        ("corpAccountKey", int(ACCOUNT_CASH as i64)),
        ("genderID", int(r.gender_id as i64)),
        ("bloodlineID", int(r.bloodline_id as i64)),
        ("raceID", int(race as i64)),
        ("hqID", opt(station)),
        ("stationid", opt(station)),
        ("solarsystemid", if station.is_none() { int(system) } else { Value::None }),
        ("solarsystemid2", int(system)),
        ("constellationid", int(r.constellation_id)),
        ("regionid", int(r.region_id)),
        ("shipid", int(ship)),
    ]);
    if station.is_none() {
        let pos = space_ship.as_ref().map(|s| [s.x.unwrap(), s.y.unwrap(), s.z.unwrap()]).unwrap_or([0.0; 3]);
        let ball = super::ship::ship_ball(c, ship, pos).await?;
        c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    }
    c.world.register_pilot(char_id, c.notify_tx.clone());
    super::online_status::presence_changed(&c.db, &c.world, char_id, true).await;
    let role = c.session.int("role").unwrap_or(0);
    c.chat.set_profile(char_id, &r.name, r.gender_id as i64, corp, alliance, None, role);
    let mut outbox = Outbox::default();
    outbox.event(GameEvent::CharacterSelected { user_id: c.login.user_id, char_id });
    dispatch_client((), outbox, c)?;
    info!(user = c.login.user_id, "character {char_id} entering game at station {station:?} / system {system}");
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    match call.method {
        // (userDetails, trainingDetails, characterDetails, wars)
        "GetCharacterSelectionData" => {
            let created = accounts::selection::creation_date(&c.db, c.login.user_id, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.map_err(db_err)?;
            let user = keyval([
                ("characterSlots", Value::Int(crate::config::game().accounts.character_slots)),
                ("maxCharacterSlots", Value::Int(crate::config::game().accounts.character_slots)),
                ("userName", Value::unicode(c.login.user_name.clone())),
                ("creationDate", Value::Int(created)),
                ("subscriptionEndTime", Value::Int(FAR_FUTURE)),
            ]);
            Ok(Value::tuple([
                Value::list([user]),
                Value::tuple([Value::None, Value::None]),
                Value::List(character_rows(c).await?),
                Value::list([]),
            ]))
        }
        "GetNumCharacters" => {
            let n = characters::selection::count(&c.db, c.login.user_id).await.map_err(db_err)?;
            Ok(Value::Int(n))
        }
        "GetCharacterLockType" => Ok(Value::None),
        // PrepareCharacterForDelete(charID) -> when it can be deleted (FILETIME).
        "PrepareCharacterForDelete" => {
            let char_id = own_character(c, call).await?;
            let delay = crate::config::game().characters.delete_delay_secs.checked_mul(FILETIME_SECOND)
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("character deletion delay exceeds FILETIME range")))?;
            let when = crate::client::filetime_now().checked_add(delay)
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("character deletion time exceeds FILETIME range")))?;
            characters::selection::prepare_delete(&c.db, char_id, when).await.map_err(db_err)?;
            info!(user = c.login.user_id, "character {char_id} submitted to the biomass queue");
            Ok(Value::Int(when))
        }
        "CancelCharacterDeletePrepare" => {
            let char_id = own_character(c, call).await?;
            characters::selection::cancel_delete(&c.db, char_id).await.map_err(db_err)?;
            Ok(Value::None)
        }
        // DeleteCharacter(charID) -> an error message key, or None once deleted.
        "DeleteCharacter" => {
            let char_id = own_character(c, call).await?;
            let deleted = characters::selection::delete(&c.db, char_id, crate::client::filetime_now()).await.map_err(db_err)?;
            if deleted == 0 {
                return Ok(Value::str(messages::MSG_CHARACTER_NOT_READY_FOR_DELETION));
            }
            info!(user = c.login.user_id, "character {char_id} deleted");
            Ok(Value::None)
        }
        "GetCharOmegaDowngradeStatus" => Ok(Value::Bool(false)),
        "SelectCharacterID" => {
            let char_id = call.args.first().and_then(|v| v.as_int()).or_else(|| call.kwargs.get("charID").and_then(|v| v.as_int()));
            let char_id = char_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("SelectCharacterID without charID")))?;
            select_character(c, char_id).await?;
            Ok(Value::None)
        }
        // gatekeeper cohorts (feature flags) the user belongs to.
        "GetCohortsForUser" => Ok(Value::list([])),

        "ValidateNameEx" => Ok(Value::Int(name_code(c, &trimmed_arg_text(call, 0)).await?)),
        "GetValidRandomName" => {
            const FIRST: [&str; 12] = [messages::RANDOM_FIRST_AREN, messages::RANDOM_FIRST_KAEL, messages::RANDOM_FIRST_SORA, messages::RANDOM_FIRST_VEYRA, messages::RANDOM_FIRST_TOMA, messages::RANDOM_FIRST_ILSA, messages::RANDOM_FIRST_DARO, messages::RANDOM_FIRST_MIRA, messages::RANDOM_FIRST_LUKAS, messages::RANDOM_FIRST_NADIA, messages::RANDOM_FIRST_JOREN, messages::RANDOM_FIRST_TESSA];
            const LAST: [&str; 12] = [messages::RANDOM_LAST_VALE, messages::RANDOM_LAST_OKARO, messages::RANDOM_LAST_THORNE, messages::RANDOM_LAST_KASSEN, messages::RANDOM_LAST_RUUN, messages::RANDOM_LAST_HALVIK, messages::RANDOM_LAST_MOREAU, messages::RANDOM_LAST_SANDOR, messages::RANDOM_LAST_IKONEN, messages::RANDOM_LAST_DRAKE, messages::RANDOM_LAST_ARVEL, messages::RANDOM_LAST_CASTOR];
            for _ in 0..RANDOM_NAME_ATTEMPTS {
                let r = crate::handshake::rand_i64() as usize;
                let name = messages::random_character_name(FIRST[r % FIRST.len()], LAST[(r / FIRST.len()) % LAST.len()], RANDOM_NAME_SUFFIX_START + (r / (FIRST.len() * LAST.len())) % RANDOM_NAME_SUFFIX_COUNT);
                if name_code(c, &name).await? == NAME_VALID {
                    return Ok(Value::unicode(name));
                }
            }
            Err(CallError::Internal(anyhow::anyhow!("could not generate a free random name")))
        }

        // (name, raceID, bloodlineID, genderID, ancestryID, appearance, portrait, schoolID, ?, qaStarterSystemID)
        "CreateCharacterWithDoll" => {
            let name = trimmed_arg_text(call, 0);
            let code = name_code(c, &name).await?;
            if code != NAME_VALID {
                return Err(CallError::User { msg: messages::MSG_CHAR_NAME_INVALID.into(), dict: vec![] });
            }
            let race_id = arg_int(call, 1).unwrap_or(FALLBACK_RACE_ID);
            let bloodline_id = arg_int(call, 2).unwrap_or(FALLBACK_BLOODLINE_ID);
            let gender_id = arg_int(call, 3).unwrap_or(0);
            let ancestry_id = arg_int(call, 4);
            let school_id = arg_int(call, 7);
            let appearance = marshal::dumps(call.args.get(5).unwrap_or(&Value::None));
            let portrait = marshal::dumps(call.args.get(6).unwrap_or(&Value::None));
            let (corp, station, system, constellation, region) = starting_location(c, school_id, race_id).await?;
            let char_id = characters::creation::create(&c.db, &characters::creation::NewCharacter {
                user_id: c.login.user_id, name: &name, race_id: race_id as i32, bloodline_id: bloodline_id as i32,
                gender_id: gender_id as i32, ancestry_id: ancestry_id.map(|v| v as i32), school_id: school_id.map(|v| v as i32),
                corporation_id: corp, station_id: station, solar_system_id: system, constellation_id: constellation, region_id: region,
                appearance: &appearance, portrait: &portrait, balance: crate::config::game().characters.starting_isk,
            }).await.map_err(db_err)?;
            super::skills::starting::grant_starting_skills(c, char_id, race_id as i32).await?;
            info!(user = c.login.user_id, "created character {name:?} ({char_id}) in station {station}");
            Ok(Value::Int(char_id))
        }
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED charUnboundMgr::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}

#[cfg(test)]
mod tests;
