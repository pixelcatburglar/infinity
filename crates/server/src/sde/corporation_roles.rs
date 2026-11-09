//! Immutable corporation role definitions from native corporation roles and groups.
//! Mirrors `evecorporation.roles`; invalid or missing static definitions prevent startup.
//! Dynamic member roles and authorization are owned by corporation services.
use crate::{dao::sde::corporation_roles as dao, db::Db};
use std::collections::HashSet;

pub(crate) struct Role { pub(crate) mask: i64, pub(crate) name: String }
pub(crate) struct Group { pub(crate) id: i64, pub(crate) mask: i64, pub(crate) scope: usize, pub(crate) divisional: bool }
#[cfg_attr(test, derive(Default))]
pub(crate) struct CorporationRoles {
    pub(crate) roles: Vec<Role>, pub(crate) groups: Vec<Group>,
    pub(crate) recognized: i64, pub(crate) global: i64, pub(crate) local: i64,
}
impl CorporationRoles {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (rows, groups) = tokio::try_join!(dao::roles(db), dao::groups(db))?;
        let mut recognized = 0i64;
        let mut roles = Vec::with_capacity(rows.len());
        for row in rows {
            anyhow::ensure!(row.key == row.embedded_key, "corporation role key mismatch");
            let bit: u32 = row.key.ok_or_else(|| anyhow::anyhow!("missing corporation role index"))?.parse()?;
            let name = row.name.filter(|name| !name.is_empty()).ok_or_else(|| anyhow::anyhow!("missing corporation role name"))?;
            anyhow::ensure!(bit < i64::BITS - 1, "invalid corporation role index");
            let mask = 1i64 << bit;
            anyhow::ensure!(recognized & mask == 0, "duplicate corporation role index {bit}");
            recognized |= mask;
            roles.push(Role { mask, name });
        }
        anyhow::ensure!(recognized != 0, "corporation roles are empty");
        let mut global = 0i64;
        let mut local = 0i64;
        let mut group_ids = HashSet::new();
        let mut parsed_groups = Vec::with_capacity(groups.len());
        for row in groups {
            anyhow::ensure!(row.key == row.embedded_key, "corporation role group key mismatch");
            let id: i64 = row.key.ok_or_else(|| anyhow::anyhow!("missing corporation role group id"))?.parse()?;
            anyhow::ensure!(group_ids.insert(id), "duplicate corporation role group");
            let mask = row.mask.ok_or_else(|| anyhow::anyhow!("missing corporation role group mask"))?;
            let divisional = match row.divisional { Some(0) => false, Some(1) => true, _ => anyhow::bail!("invalid role division") };
            anyhow::ensure!(mask > 0, "invalid corporation role group mask");
            let scope = match (row.applies.as_deref(), row.grantable.as_deref(), row.locational) {
                (Some("roles"), Some("grantableRoles"), Some(0)) => { global |= mask; 0 },
                (Some("rolesAtHQ"), Some("grantableRolesAtHQ"), Some(1)) => { local |= mask; 1 },
                (Some("rolesAtBase"), Some("grantableRolesAtBase"), Some(1)) => { local |= mask; 2 },
                (Some("rolesAtOther"), Some("grantableRolesAtOther"), Some(1)) => { local |= mask; 3 },
                _ => anyhow::bail!("invalid corporation role group scope"),
            };
            parsed_groups.push(Group { id, mask, scope, divisional });
        }
        let mut per_scope = [0i64; 4];
        for group in &parsed_groups { per_scope[group.scope] |= group.mask; }
        anyhow::ensure!(per_scope[1] != 0 && per_scope[1] == per_scope[2] && per_scope[1] == per_scope[3], "inconsistent location role groups");
        anyhow::ensure!(global != 0 && local != 0 && global & local == 0, "invalid corporation role scope partition");
        global |= recognized & !local;
        for role in &roles { tracing::debug!(role = %role.name, mask = role.mask, "loaded corporation role"); }
        for group in &parsed_groups { tracing::debug!(group = group.id, divisional = group.divisional, "loaded corporation role group"); }
        Ok(Self { roles, groups: parsed_groups, recognized, global, local })
    }
    /// Roles present in the client dataset for each scope, excluding unrecognized legacy bits.
    pub(crate) fn scope_masks(&self) -> [i64; 4] {
        debug_assert_eq!(self.roles.len(), self.recognized.count_ones() as usize);
        let mut masks = [self.global & self.recognized, 0, 0, 0];
        for group in &self.groups {
            if group.scope != 0 { masks[group.scope] |= group.mask & self.recognized; }
        }
        masks
    }
}
