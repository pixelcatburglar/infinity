//! Immutable startup character creation and NPC identity data.
//! This dataset has no direct client mirror; character selection and skill services consume it.
//! Sources are typed races, bloodlines and NPC characters and native schools.
//! Other character and school fields are not retained.

use std::collections::HashMap;

use crate::{dao, db::Db};

pub(crate) struct StartingSkill { pub type_id: i32, pub level: i32 }
pub(crate) struct Race { pub race_id: i64, pub ship_type_id: Option<i32>, pub skills: Vec<StartingSkill> }
pub(crate) struct Bloodline {
    pub bloodline_id: i64, pub charisma: Option<i32>, pub intelligence: Option<i32>,
    pub memory: Option<i32>, pub perception: Option<i32>, pub willpower: Option<i32>,
}
pub(crate) struct NpcCharacter {
    pub character_id: i64, pub corporation_id: Option<i64>, pub name: Option<String>,
    pub gender: Option<i32>, pub name_id: Option<i64>,
}
pub(crate) struct School {
    pub school_id: i64, pub race_id: Option<i32>, pub corporation_id: Option<i64>,
    pub starting_stations: Vec<i64>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct Characters {
    races: Vec<Race>, bloodlines: Vec<Bloodline>, npc_characters: Vec<NpcCharacter>, schools: Vec<School>,
    schools_by_race: HashMap<i32, Vec<usize>>,
}

impl Characters {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (mut races, skills, bloodlines, npc_characters, schools) = tokio::try_join!(
            dao::sde::characters::races(db), dao::sde::characters::starting_skills(db),
            dao::sde::characters::bloodlines(db), dao::sde::characters::npc_characters(db),
            dao::sde::characters::schools(db),
        )?;
        for (race_id, skill) in skills {
            if let Ok(index) = races.binary_search_by_key(&race_id, |race| race.race_id) {
                races[index].skills.push(skill);
            }
        }
        let mut schools_by_race: HashMap<i32, Vec<usize>> = HashMap::new();
        for (index, school) in schools.iter().enumerate() {
            if let Some(race_id) = school.race_id {
                schools_by_race.entry(race_id).or_default().push(index);
            }
        }
        Ok(Self { races, bloodlines, npc_characters, schools, schools_by_race })
    }

    pub(crate) fn race(&self, id: i64) -> Option<&Race> {
        self.races.binary_search_by_key(&id, |row| row.race_id).ok().map(|index| &self.races[index])
    }
    pub(crate) fn races(&self) -> impl Iterator<Item = &Race> { self.races.iter() }
    pub(crate) fn bloodline(&self, id: i64) -> Option<&Bloodline> {
        self.bloodlines.binary_search_by_key(&id, |row| row.bloodline_id).ok().map(|index| &self.bloodlines[index])
    }
    pub(crate) fn npc_character(&self, id: i64) -> Option<&NpcCharacter> {
        self.npc_characters.binary_search_by_key(&id, |row| row.character_id).ok().map(|index| &self.npc_characters[index])
    }
    pub(crate) fn school(&self, id: i64) -> Option<&School> {
        self.schools.binary_search_by_key(&id, |row| row.school_id).ok().map(|index| &self.schools[index])
    }
    pub(crate) fn schools_for_race(&self, race: i32) -> impl Iterator<Item = &School> {
        self.schools_by_race.get(&race).into_iter().flatten().map(|&index| &self.schools[index])
    }
    pub(crate) fn npc_characters(&self) -> impl Iterator<Item = &NpcCharacter> { self.npc_characters.iter() }
}
