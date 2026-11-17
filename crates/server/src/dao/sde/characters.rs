//! Startup projections for character creation and NPC identities.
//! This DAO has no direct client mirror; character selection and skill services consume the data.
//! Sources: typed `sde.chr_races`, `sde.chr_bloodlines`, `sde.npc_characters` and `sde_native.schools`.
//! NPC rows also project the agent marker and deletion flag; other character and school fields are not loaded.

use crate::{db::{executor::Executor, Error}, sde::characters::{Bloodline, NpcCharacter, Race, School, StartingSkill}};
use tokio_postgres::Row;

impl Race {
    fn from_row(row: &Row) -> Self {
        Self { race_id: row.get("race_id"), ship_type_id: row.get("ship_type_id"), skills: Vec::new() }
    }
}
struct StartingSkillRow { race_id: i64, skill: StartingSkill }
impl StartingSkillRow {
    fn from_row(row: &Row) -> Self {
        Self { race_id: row.get("race_id"), skill: StartingSkill { type_id: row.get("type_id"), level: row.get("level") } }
    }
}
impl Bloodline {
    fn from_row(row: &Row) -> Self {
        Self { bloodline_id: row.get("bloodline_id"), charisma: row.get("charisma"), intelligence: row.get("intelligence"), memory: row.get("memory"), perception: row.get("perception"), willpower: row.get("willpower") }
    }
}
impl NpcCharacter {
    fn from_row(row: &Row) -> Self {
        Self { character_id: row.get("character_id"), corporation_id: row.get("corporation_id"), name: row.get("name"), gender: row.get("gender"), name_id: row.get("name_id"), is_agent: row.get("is_agent"), deleted: row.get("deleted") }
    }
}
impl School {
    fn from_row(row: &Row) -> Self {
        Self { school_id: row.get("school_id"), race_id: row.get("race_id"), corporation_id: row.get("corporation_id"), starting_stations: row.get("starting_stations") }
    }
}

pub(crate) async fn races(ex: &(impl Executor + ?Sized)) -> Result<Vec<Race>, Error> {
    Ok(ex.query("SELECT race_id::bigint AS race_id, ship_type_id FROM sde.chr_races ORDER BY race_id", &[]).await?.iter().map(Race::from_row).collect())
}
pub(crate) async fn starting_skills(ex: &(impl Executor + ?Sized)) -> Result<Vec<(i64, StartingSkill)>, Error> {
    Ok(ex.query("SELECT r.race_id::bigint AS race_id, (s.value->>'_key')::int AS type_id, (s.value->>'_value')::int AS level FROM sde.chr_races r CROSS JOIN LATERAL jsonb_array_elements(r.data->'skills') WITH ORDINALITY AS s(value, ordinal) ORDER BY r.race_id, s.ordinal", &[]).await?.iter().map(StartingSkillRow::from_row).map(|row| (row.race_id, row.skill)).collect())
}
pub(crate) async fn bloodlines(ex: &(impl Executor + ?Sized)) -> Result<Vec<Bloodline>, Error> {
    Ok(ex.query("SELECT bloodline_id::bigint AS bloodline_id, charisma, intelligence, memory, perception, willpower FROM sde.chr_bloodlines ORDER BY bloodline_id", &[]).await?.iter().map(Bloodline::from_row).collect())
}
pub(crate) async fn npc_characters(ex: &(impl Executor + ?Sized)) -> Result<Vec<NpcCharacter>, Error> {
    Ok(ex.query("SELECT character_id::bigint AS character_id, (data->>'corporationID')::bigint AS corporation_id, data->'name'->>'en' AS name, (data->>'gender')::boolean::int AS gender, (data->>'nameID')::bigint AS name_id, COALESCE(jsonb_typeof(data->'agent') = 'object', false) AS is_agent, COALESCE((data->>'deleted')::boolean, false) AS deleted FROM sde.npc_characters ORDER BY character_id", &[]).await?.iter().map(NpcCharacter::from_row).collect())
}
pub(crate) async fn schools(ex: &(impl Executor + ?Sized)) -> Result<Vec<School>, Error> {
    Ok(ex.query("SELECT key::bigint AS school_id, (data->>'raceID')::int AS race_id, (data->>'corporationID')::bigint AS corporation_id, ARRAY(SELECT station::bigint FROM jsonb_array_elements_text(data->'startingStations') WITH ORDINALITY AS s(station, ordinal) ORDER BY ordinal) AS starting_stations FROM sde_native.schools ORDER BY key::bigint", &[]).await?.iter().map(School::from_row).collect())
}
