//! Immutable native wreck mappings used by NPC combat.
//! No direct client mirror; `sde_native.wrecks` supplies group and race mappings.
//! Wreck dimensions come from canonical types; other wreck fields are unsupported.
use std::collections::HashMap;
use crate::{dao::sde::wrecks as dao, db::Db};

#[cfg_attr(test, derive(Default))]
pub(crate) struct Wrecks { mappings: HashMap<i32, HashMap<i32, i32>> }
impl Wrecks {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let mut mappings: HashMap<i32, HashMap<i32, i32>> = HashMap::new();
        for row in dao::load(db).await? {
            if let (Ok(group), Some(race), Some(kind)) = (row.group_key.parse::<i32>(), row.race_key, row.type_id) {
                if let Ok(race_id) = race.parse::<i32>() {
                    if group.to_string() == row.group_key && race_id.to_string() == race {
                        mappings.entry(group).or_default().insert(race_id, kind.parse()?);
                    }
                }
            }
        }
        Ok(Self { mappings })
    }
    pub(crate) fn get(&self, group: i32, race: i32) -> Option<i32> {
        let races = self.mappings.get(&group)?;
        races.get(&race).or_else(|| races.get(&0)).copied()
    }
}

#[cfg(test)] mod tests;
