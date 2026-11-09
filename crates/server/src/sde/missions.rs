//! Immutable projected mission and epic-arc data for agent services.
//! The client consumes epic journal messages via `evemissions.common.epic_arcs_data`.
//! Sources are `sde_native.missions` and `sde_native.epicarcs`; runtime mission state is excluded.

use std::collections::HashSet;
use crate::{dao, db::Db};

pub(crate) struct Mission {
    pub mission_id: i32,
    pub name_id: Option<i64>,
    pub has_name: bool,
    pub template: Option<String>,
    pub dungeon_id: Option<i32>,
    pub faction_id: Option<i64>,
    pub cargo_type: Option<i32>,
    pub cargo_qty: Option<i32>,
    pub reward_type: Option<i32>,
    pub reward_quantity: Option<i64>,
    pub reward_lp: Option<i64>,
    pub(crate) messages: Vec<JournalMessage>,
}
impl Mission {
    pub(crate) fn message(&self, key: &str) -> Option<i64> { self.messages.binary_search_by(|message| message.key.as_str().cmp(key)).ok().and_then(|i| self.messages[i].message_id) }
    pub(crate) fn has_message(&self, key: &str) -> bool { self.messages.binary_search_by(|message| message.key.as_str().cmp(key)).is_ok() }
}

pub(crate) struct ArcStep {
    pub mission_id: i32,
    pub arc: i64,
    pub agent: i64,
    pub next: Vec<i32>,
    pub fail: Option<i32>,
    pub start: bool,
}
pub(crate) struct JournalMessage {
    pub key: String,
    pub mission_id: i32,
    pub message_id: Option<i64>,
}
#[cfg_attr(test, derive(Default))]
pub(crate) struct Missions {
    rows: Vec<Mission>,
    steps: Vec<ArcStep>,
    journal: Vec<(usize, usize)>,
}
impl Missions {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (mut rows, messages, mut steps) = tokio::try_join!(
            dao::sde::missions::missions(db), dao::sde::missions::messages(db), dao::sde::missions::steps(db),
        )?;
        for message in messages {
            if let Ok(index) = rows.binary_search_by_key(&message.mission_id, |row| row.mission_id) {
                rows[index].messages.push(message);
            }
        }
        let mut journal = Vec::new();
        for (mission_position, mission) in rows.iter_mut().enumerate() {
            mission.messages.sort_unstable_by(|a, b| a.key.cmp(&b.key));
            for (message_position, message) in mission.messages.iter().enumerate() {
                if message.key.starts_with("messages.epicMission.journalText.") {
                    journal.push((mission_position, message_position));
                }
            }
        }
        journal.sort_unstable_by(|&(am, ai), &(bm, bi)| {
            let a = &rows[am].messages[ai];
            let b = &rows[bm].messages[bi];
            a.key.cmp(&b.key).then(a.mission_id.cmp(&b.mission_id))
        });
        let followers: HashSet<i32> = steps.iter().flat_map(|step| step.next.iter().copied()).collect();
        for step in &mut steps { step.start = !followers.contains(&step.mission_id); }
        Ok(Self { rows, steps, journal })
    }
    pub(crate) fn get(&self, id: i32) -> Option<&Mission> { self.rows.binary_search_by_key(&id, |row| row.mission_id).ok().map(|i| &self.rows[i]) }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Mission> { self.rows.iter() }
    pub(crate) fn step(&self, id: i32) -> Option<&ArcStep> { self.steps.binary_search_by_key(&id, |step| step.mission_id).ok().map(|i| &self.steps[i]) }
    pub(crate) fn steps(&self) -> impl Iterator<Item = &ArcStep> { self.steps.iter() }
    pub(crate) fn journal(&self) -> impl Iterator<Item = &JournalMessage> { self.journal.iter().map(|&(mission, message)| &self.rows[mission].messages[message]) }
}
