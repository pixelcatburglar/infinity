//! charUnboundMgr: character selection and creation before a character is
//! chosen (loginCharacterselection/charselData.py, eve/client/script/ui/login).
//! Uses character, account, item and skill DAOs and SDE starting data; appearance is stored without interpretation.
//! Slot counts, name limits, starting ISK and deletion delay are operator policies.
//! Selecting a character registers process-local presence; appearance editing is not interpreted here.
//! Selection reconciles the active fit before restoring the ship's persisted condition.
//! Ships restored in space arrive with the actor-managed restoring cloak and timer.
//! Selection hands off its lifecycle event after session changes; logoff commits with presence cleanup.
//! First selection serializes rookie ship grants on the character row and grants a
//! persistent server-selected civilian fit without charges; existing ships are not retroactively refitted.
//! Final deletion is serialized with market writes and refuses open personal orders.

use crate::dao::{accounts, characters, skills};
use super::character::creation::{self, NewPilot, NAME_VALID};
use super::{db_err, keyval, trimmed_arg_text, Call, CallError, CallResult};
use crate::client::{short, Client};
use infinity_core::consts::{ACCOUNT_CASH, FALLBACK_ROOKIE_SHIP, FAR_FUTURE, FILETIME_SECOND, FILETIME_UNIX_EPOCH};
use crate::db::outbox::Outbox;
use crate::lua::events::GameEvent;
use infinity_core::messages;
use marshal::Value;
use tracing::{info, warn};

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
    let cleared = [
        "charid", "corpid", "corprole", "rolesAtAll", "rolesAtBase", "rolesAtHQ", "rolesAtOther", "corpAccountKey", "allianceid", "warfactionid",
        "genderID", "bloodlineID", "raceID", "hqID", "stationid", "solarsystemid", "solarsystemid2", "constellationid", "regionid", "shipid",
    ];
    c.change_session(cleared.into_iter().map(|k| {
        let value = match k {
            "corprole" | "rolesAtAll" | "rolesAtBase" | "rolesAtHQ" | "rolesAtOther" => Value::Int(0),
            _ => Value::None,
        };
        (k, value)
    }).collect());
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

/// Select a character and restore its station or space session (eveSessions.GetSessionValuesFromRowset).
async fn select_character(c: &mut Client, char_id: i64) -> Result<(), CallError> {
    let user = c.login.user_id;
    let r = c.db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            characters::selection::selected(tx, char_id, user).await.map_err(db_err)
        }),
        |value, ()| async move { Ok(value) },
    ).await?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
    super::ship_cosmetics_mgr::expiry::converge(&c.db, &c.world, char_id, crate::client::filetime_now()).await?;
    let station = r.station_id;
    let system = r.solar_system_id;
    let race = r.race_id;
    let super::presence::Prepared { ship_id: ship, cloak_until } = super::presence::prepare(
        &c.db, &c.world, &c.sde, char_id, &r,
        |n| super::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid")),
    ).await?;
    let int = Value::Int;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    let mut corp = r.corporation_id;
    let roles = r.roles;
    let authority = &r.authority;
    let mut alliance = r.alliance_id;
    c.change_session(vec![
        ("charid", int(char_id)),
        ("corpid", int(corp)),
        ("allianceid", opt(alliance)),
        ("corprole", int(roles)),
        ("rolesAtAll", int(roles)),
        ("rolesAtBase", int(authority.roles[2])),
        ("rolesAtHQ", int(authority.roles[1])),
        ("rolesAtOther", int(authority.roles[3])),
        ("corpAccountKey", int(ACCOUNT_CASH as i64)),
        ("genderID", int(r.gender_id as i64)),
        ("bloodlineID", int(r.bloodline_id as i64)),
        ("raceID", int(race as i64)),
        ("hqID", opt(authority.headquarters.or_else(|| c.sde.npc_corporations.get(corp).and_then(|row| row.station_id)))),
        ("stationid", opt(station)),
        ("solarsystemid", if station.is_none() { int(system) } else { Value::None }),
        ("solarsystemid2", int(system)),
        ("constellationid", int(r.constellation_id)),
        ("regionid", int(r.region_id)),
        ("shipid", int(ship)),
    ]);
    // Re-read after publication so a concurrent corporation move cannot leave
    // the newly registered session carrying its pre-publication membership.
    let fresh = characters::selection::selected(&c.db, char_id, user).await.map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
    corp = fresh.corporation_id;
    alliance = fresh.alliance_id;
    let current = &fresh.authority;
    let hq = current.headquarters.or_else(|| c.sde.npc_corporations.get(corp).and_then(|row| row.station_id));
    let values = [
        ("corpid", Some(corp)), ("allianceid", alliance), ("corprole", Some(current.roles[0])),
        ("rolesAtAll", Some(current.roles[0])), ("rolesAtHQ", Some(current.roles[1])),
        ("rolesAtBase", Some(current.roles[2])), ("rolesAtOther", Some(current.roles[3])), ("hqID", hq),
    ];
    let updates: Vec<_> = values.into_iter().filter(|(name, value)| c.session.int(name) != *value)
        .map(|(name, value)| (name, opt(value))).collect();
    if !updates.is_empty() { c.change_session(updates); }
    characters::selection::logon(&c.db, char_id).await.map_err(db_err)?;
    let now = crate::client::filetime_now();
    let active = skills::expert_systems::active(&c.db, char_id, now).await.map_err(db_err)?;
    let active = super::skills::expert_systems::map(&active);
    c.notify("OnExpertSystemsUpdated", vec![active, Value::Bool(false), Value::None]);
    if let Some(cloak_until) = cloak_until {
        super::ship::notify_jump_cloak(c, ship, cloak_until);
    }
    super::online_status::presence_changed(&c.db, &c.world, char_id, true).await;
    let role = c.session.int("role").unwrap_or(0);
    c.chat.set_profile(char_id, &r.name, r.gender_id as i64, corp, alliance, None, role);
    let world = c.world.clone();
    let user_id = c.login.user_id;
    if let Err(error) = c.db.transaction(
        |_, outbox: &mut Outbox| Box::pin(async move {
            outbox.event(GameEvent::CharacterSelected { user_id, char_id });
            Ok::<_, CallError>(())
        }),
        |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &world) },
    ).await {
        tracing::warn!(?error, char_id, "character selection event dispatch failed");
    }
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
            let user_id = c.login.user_id;
            let now = crate::client::filetime_now();
            let _market = super::market::lock().await;
            let db = c.db.clone();
            let result = db.transaction(
                |tx, _: &mut Outbox| Box::pin(async move {
                    if !characters::selection::owns(tx, char_id, user_id).await.map_err(db_err)? {
                        return Err(CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] });
                    }
                    if crate::dao::market::orders::has_open_owner_orders(tx, char_id).await.map_err(db_err)?
                        || characters::selection::delete(tx, char_id, now).await.map_err(db_err)? == 0 {
                        return Ok(Value::str(messages::MSG_CHARACTER_NOT_READY_FOR_DELETION));
                    }
                    Ok(Value::None)
                }),
                |value, outbox| async move { crate::db::outbox::dispatch_client(value, outbox, c) },
            ).await?;
            if matches!(result, Value::None) {
                info!(user = user_id, "character {char_id} deleted");
            }
            Ok(result)
        }
        // The client consumes a historical lapsed-Omega flag; current authority
        // tracks eligibility, not whether a downgrade occurred previously.
        "GetCharOmegaDowngradeStatus" => Ok(Value::Bool(false)),
        "SelectCharacterID" => {
            let char_id = call.args.first().and_then(|v| v.as_int()).or_else(|| call.kwargs.get("charID").and_then(|v| v.as_int()));
            let char_id = char_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("SelectCharacterID without charID")))?;
            select_character(c, char_id).await?;
            Ok(Value::None)
        }
        // gatekeeper cohorts (feature flags) the user belongs to.
        "GetCohortsForUser" => Ok(Value::list([])),

        "ValidateNameEx" => Ok(Value::Int(creation::name_code(&c.db, &trimmed_arg_text(call, 0)).await?)),
        "GetValidRandomName" => {
            const FIRST: [&str; 12] = [messages::RANDOM_FIRST_AREN, messages::RANDOM_FIRST_KAEL, messages::RANDOM_FIRST_SORA, messages::RANDOM_FIRST_VEYRA, messages::RANDOM_FIRST_TOMA, messages::RANDOM_FIRST_ILSA, messages::RANDOM_FIRST_DARO, messages::RANDOM_FIRST_MIRA, messages::RANDOM_FIRST_LUKAS, messages::RANDOM_FIRST_NADIA, messages::RANDOM_FIRST_JOREN, messages::RANDOM_FIRST_TESSA];
            const LAST: [&str; 12] = [messages::RANDOM_LAST_VALE, messages::RANDOM_LAST_OKARO, messages::RANDOM_LAST_THORNE, messages::RANDOM_LAST_KASSEN, messages::RANDOM_LAST_RUUN, messages::RANDOM_LAST_HALVIK, messages::RANDOM_LAST_MOREAU, messages::RANDOM_LAST_SANDOR, messages::RANDOM_LAST_IKONEN, messages::RANDOM_LAST_DRAKE, messages::RANDOM_LAST_ARVEL, messages::RANDOM_LAST_CASTOR];
            for _ in 0..RANDOM_NAME_ATTEMPTS {
                let r = crate::handshake::rand_i64() as usize;
                let name = messages::random_character_name(FIRST[r % FIRST.len()], LAST[(r / FIRST.len()) % LAST.len()], RANDOM_NAME_SUFFIX_START + (r / (FIRST.len() * LAST.len())) % RANDOM_NAME_SUFFIX_COUNT);
                if creation::name_code(&c.db, &name).await? == NAME_VALID {
                    return Ok(Value::unicode(name));
                }
            }
            Err(CallError::Internal(anyhow::anyhow!("could not generate a free random name")))
        }

        // (name, raceID, bloodlineID, genderID, ancestryID, appearance, portrait, schoolID, ?, qaStarterSystemID)
        "CreateCharacterWithDoll" => {
            let name = trimmed_arg_text(call, 0);
            let race_id = arg_int(call, 1).unwrap_or(FALLBACK_RACE_ID);
            let bloodline_id = arg_int(call, 2).unwrap_or(FALLBACK_BLOODLINE_ID);
            let gender_id = arg_int(call, 3).unwrap_or(0);
            let ancestry_id = arg_int(call, 4);
            let school_id = arg_int(call, 7);
            let appearance = marshal::dumps(call.args.get(5).unwrap_or(&Value::None));
            let portrait = marshal::dumps(call.args.get(6).unwrap_or(&Value::None));
            let created = creation::create(&c.db, &c.sde, c.login.user_id, NewPilot {
                name, race: race_id, bloodline: bloodline_id, gender: gender_id,
                ancestry: ancestry_id, school: school_id, station: None, appearance, portrait,
                balance: crate::config::game().characters.starting_isk,
            }).await?;
            let char_id = created.char_id;
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
