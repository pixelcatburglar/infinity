//! Focused trained-skill reads for fleet-warp formation admission from `game.skills`.
//! This DAO has no direct client mirror; wire decoding, tier policy and refusals belong
//! to the owning service. Skill type IDs arrive as parameters.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

/// Trained levels of the two formation-gating skills for one character.
pub(crate) struct FormationLevels {
    pub(crate) formation: i32,
    pub(crate) coordination: i32,
}
impl FormationLevels {
    fn from_row(r: &Row) -> Self {
        Self { formation: r.get("formation_level"), coordination: r.get("coordination_level") }
    }
}
/// Highest trained level of each requested skill; untrained skills report 0.
pub(crate) async fn trained_levels(
    ex: &(impl Executor + ?Sized), character: i64, formation_type: i32, coordination_type: i32,
) -> Result<FormationLevels, db::Error> {
    Ok(FormationLevels::from_row(&ex.query_one(
        "SELECT COALESCE(MAX(level) FILTER (WHERE type_id = $2), 0) AS formation_level, \
         COALESCE(MAX(level) FILTER (WHERE type_id = $3), 0) AS coordination_level \
         FROM game.skills WHERE character_id = $1 AND type_id IN ($2, $3)",
        &[&character, &formation_type, &coordination_type],
    ).await?))
}
