//! SQL expression combining character role bits with held titles in `game.corp_titles`.
//! The caller supplies the character row; this DAO has no direct client mirror or wire contract.
//! Owning services supply client contracts and handle role policy, transactions, refusals, notifications and World.

/// Server title model: union the member's role bits with the roles of all held titles.
pub(crate) const EFFECTIVE_ROLES_SQL: &str = "(ch.corp_roles | COALESCE((SELECT bit_or(t.roles) FROM game.corp_titles t
     WHERE t.corporation_id = ch.corporation_id AND NOT ch.corp_block_roles AND ch.corp_title_mask & t.title_id <> 0), 0))";

/// Shared current corporation join; a missing or deleted corporation has no live row.
pub(crate) const CURRENT_CORPORATION_JOIN_SQL: &str = "FROM game.characters ch LEFT JOIN game.corporations co ON co.corporation_id=ch.corporation_id AND NOT co.deleted";

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Authority {
    pub(crate) character: i64, pub(crate) corporation: i64, pub(crate) ceo: Option<i64>, pub(crate) headquarters: Option<i64>,
    pub(crate) live: bool,
    pub(crate) base: Option<i64>, pub(crate) blocked: bool,
    pub(crate) roles: [i64; 4], pub(crate) grantable: [i64; 4],
}
impl Authority {
    pub(super) fn from_row(r: &Row) -> Self {
        Self { character: r.get("character_id"), corporation: r.get("corporation_id"), ceo: r.get("ceo_id"), headquarters: r.get("headquarters"), live: r.get("corporation_live"),
            base: r.get("corp_base_id"), blocked: r.get("corp_block_roles"),
            roles: [r.get("roles"), r.get("roles_hq"), r.get("roles_base"), r.get("roles_other")],
            grantable: [r.get("grantable"), r.get("grantable_hq"), r.get("grantable_base"), r.get("grantable_other")] }
    }
}
pub(super) fn projection(predicate: &str) -> String {
    format!("SELECT ch.character_id, ch.corporation_id, co.ceo_id, co.station_id AS headquarters, (co.corporation_id IS NOT NULL) AS corporation_live, ch.corp_base_id, ch.corp_block_roles,
        {EFFECTIVE_ROLES_SQL} AS roles,
        ch.corp_roles_hq | COALESCE(t.hq,0) AS roles_hq,
        ch.corp_roles_base | COALESCE(t.base,0) AS roles_base,
        ch.corp_roles_other | COALESCE(t.other,0) AS roles_other,
        ch.corp_grantable_roles | COALESCE(t.grantable,0) AS grantable,
        ch.corp_grantable_hq | COALESCE(t.grantable_hq,0) AS grantable_hq,
        ch.corp_grantable_base | COALESCE(t.grantable_base,0) AS grantable_base,
        ch.corp_grantable_other | COALESCE(t.grantable_other,0) AS grantable_other
        {CURRENT_CORPORATION_JOIN_SQL}
        LEFT JOIN LATERAL (SELECT bit_or(roles_hq) AS hq, bit_or(roles_base) AS base, bit_or(roles_other) AS other,
            bit_or(grantable_roles) AS grantable, bit_or(grantable_hq) AS grantable_hq,
            bit_or(grantable_base) AS grantable_base, bit_or(grantable_other) AS grantable_other
            FROM game.corp_titles t WHERE t.corporation_id=ch.corporation_id AND NOT ch.corp_block_roles AND ch.corp_title_mask & t.title_id <> 0) t ON true
        WHERE {predicate} AND NOT ch.deleted")
}
pub(crate) async fn authority(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<Authority>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| projection("ch.character_id=$1"));
    Ok(ex.query_opt(SQL.as_str(), &[&character]).await?.as_ref().map(Authority::from_row))
}
pub(crate) async fn authorities(ex: &(impl Executor + ?Sized), characters: &[i64]) -> Result<Vec<Authority>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| projection("ch.character_id=ANY($1)"));
    Ok(ex.query(SQL.as_str(), &[&characters]).await?.iter().map(Authority::from_row).collect())
}
