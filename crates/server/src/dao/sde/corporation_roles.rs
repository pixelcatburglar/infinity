//! Startup-only corporation role definitions from native static tables.
//! The client loads the same role and group datasets through `evecorporation.roles`.
//! Runtime corporation state and authorization are not handled here.
use crate::db::{executor::Executor, Error};

pub(crate) struct RoleRow { pub(crate) key: Option<String>, pub(crate) embedded_key: Option<String>, pub(crate) name: Option<String> }
pub(crate) struct GroupRow {
    pub(crate) key: Option<String>,
    pub(crate) embedded_key: Option<String>,
    pub(crate) mask: Option<i64>,
    pub(crate) applies: Option<String>,
    pub(crate) grantable: Option<String>,
    pub(crate) locational: Option<i32>,
    pub(crate) divisional: Option<i32>,
}

pub(crate) async fn roles(ex: &(impl Executor + ?Sized)) -> Result<Vec<RoleRow>, Error> {
    Ok(ex.query("SELECT key, data->>'_key' AS embedded_key, data->>'roleName' AS name FROM sde_native.corporationroles ORDER BY row_no", &[]).await?
        .iter().map(|r| RoleRow { key: r.get("key"), embedded_key: r.get("embedded_key"), name: r.get("name") }).collect())
}
pub(crate) async fn groups(ex: &(impl Executor + ?Sized)) -> Result<Vec<GroupRow>, Error> {
    Ok(ex.query("SELECT key, data->>'_key' AS embedded_key, (data->>'roleMask')::bigint AS mask, data->>'appliesTo' AS applies, data->>'appliesToGrantable' AS grantable, (data->>'isLocational')::int AS locational, (data->>'isDivisional')::int AS divisional FROM sde_native.corporationrolegroups ORDER BY row_no", &[]).await?
        .iter().map(|r| GroupRow { key: r.get("key"), embedded_key: r.get("embedded_key"), mask: r.get("mask"), applies: r.get("applies"), grantable: r.get("grantable"), locational: r.get("locational"), divisional: r.get("divisional") }).collect())
}
