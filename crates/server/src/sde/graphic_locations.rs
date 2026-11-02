//! Immutable native NPC station graphic directional locators indexed by type.
//! `fsdBuiltData/common/graphicLocations.py:GetDirectionalLocators` supplies
//! `ballparkCommon/docking.py:GetUndockDirections`. Missing locators use client fallback;
//! malformed locators fail startup. Model rotations and non-directional locators are not represented.
use std::collections::HashMap;
use crate::{dao::sde::graphic_locations as dao, db::Db, space::V3};

pub(crate) struct DirectionalLocator {
    pub(crate) category: String,
    pub(crate) position: V3,
    pub(crate) direction: V3,
}

pub(crate) struct GraphicLocations(HashMap<i32, Vec<DirectionalLocator>>);

impl GraphicLocations {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Self::from_rows(dao::load(db).await?)
    }

    fn from_rows(rows: Vec<dao::GraphicLocationRow>) -> anyhow::Result<Self> {
        let mut by_type = HashMap::new();
        for row in rows {
            let value: serde_json::Value = serde_json::from_str(&row.directional_locators)?;
            let entries = match &value {
                serde_json::Value::Null => continue,
                serde_json::Value::Array(entries) => entries,
                _ => anyhow::bail!("invalid directional locators for type {}", row.type_id),
            };
            let mut locators = Vec::with_capacity(entries.len());
            for entry in entries {
                let category = entry.get("category").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("invalid locator category for type {}", row.type_id))?;
                let vector = |name: &str| -> anyhow::Result<V3> {
                    let array = entry.get(name).and_then(|v| v.as_array())
                        .filter(|a| a.len() == 3).ok_or_else(|| anyhow::anyhow!("invalid locator {name} for type {}", row.type_id))?;
                    let component = |i: usize| array[i].as_f64().filter(|v| v.is_finite())
                        .ok_or_else(|| anyhow::anyhow!("invalid locator {name} for type {}", row.type_id));
                    Ok([component(0)?, component(1)?, component(2)?])
                };
                let position = vector("position")?;
                let direction = vector("direction")?;
                anyhow::ensure!(direction != [0.0; 3], "zero locator direction for type {}", row.type_id);
                locators.push(DirectionalLocator { category: category.to_owned(), position, direction });
            }
            by_type.insert(row.type_id, locators);
        }
        Ok(Self(by_type))
    }

    pub(crate) fn get(&self, type_id: i32) -> &[DirectionalLocator] {
        self.0.get(&type_id).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[cfg(test)] mod tests;
