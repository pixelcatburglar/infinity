//! Immutable industry blueprint activities and station services from typed SDE tables.
//! Consumers mirror `eve/client/script/industry`; unsupported activities are not loaded.
//! Native blueprint and station-operation tables are deliberately not used.

use std::collections::HashMap;
use crate::{dao::sde::industry as dao, db::{self, executor::Executor}};

/// Activity IDs from the client `industry.const`.
pub(crate) const MANUFACTURING: i32 = 1;
pub(crate) const RESEARCH_TIME: i32 = 3;
pub(crate) const RESEARCH_MATERIAL: i32 = 4;
pub(crate) const COPYING: i32 = 5;
pub(crate) const INVENTION: i32 = 8;

pub(crate) struct Activity {
    pub time: f64,
    pub materials: Vec<(i32, i64)>,
    pub products: Vec<(i32, i64, f64)>,
    pub skills: Vec<(i32, i32)>,
    pub max_production_limit: i32,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct Industry {
    activities: HashMap<(i32, i32), Activity>,
    operations: HashMap<i64, Vec<i32>>,
}

impl Industry {
    pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Self, db::Error> {
        let (heads, parts, operations) = tokio::try_join!(dao::activities(ex), dao::parts(ex), dao::operations(ex))?;
        let mut activities: HashMap<_, _> = heads.into_iter().map(|row| ((row.blueprint, row.activity), Activity {
            time: row.time.unwrap_or(0.0), max_production_limit: row.max_production_limit,
            materials: Vec::new(), products: Vec::new(), skills: Vec::new(),
        })).collect();
        for row in parts {
            if let Some(activity) = activities.get_mut(&(row.blueprint, row.activity)) {
                match row.part.as_str() {
                    "materials" => activity.materials.push((row.type_id, row.quantity)),
                    "products" => activity.products.push((row.type_id, row.quantity, row.probability)),
                    "skills" => activity.skills.push((row.type_id, row.level)),
                    _ => unreachable!(),
                }
            }
        }
        Ok(Self { activities, operations: operations.into_iter().map(|r| (r.operation_id, r.services)).collect() })
    }

    pub(crate) fn activity(&self, blueprint: i32, activity: i32) -> Option<&Activity> {
        self.activities.get(&(blueprint, activity))
    }

    pub(crate) fn has_service(&self, operation: Option<i64>, service: i32) -> bool {
        operation.and_then(|id| self.operations.get(&id)).is_some_and(|ids| ids.contains(&service))
    }
}
