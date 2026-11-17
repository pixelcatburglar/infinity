//! War legality and immutable kill-side classification for Crimewatch, mirroring
//! `evewar/util.py` and `eve/client/script/ui/services/wars/war_cso.py`.
//! Uses `game.wars`, `game.killmail_wars` and corporation/alliance affiliation snapshots.
//! Aggressive wars, allies and paid treaties are unsupported; historical affiliations are never reconstructed.

use crate::dao::{corporations::roles, killmails::wars as projections, wars};
use crate::db::executor::Executor;
use crate::services::{db_err, CallError};
use std::collections::HashSet;

pub(crate) struct Affiliation {
    pub character: i64,
    pub corporation: i64,
    pub alliance: Option<i64>,
}

async fn affiliation(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<(i64, Option<i64>)>, CallError> {
    Ok(roles::info(ex, character).await.map_err(db_err)?.map(|info| (info.alliance_id.unwrap_or(info.corporation_id), info.alliance_id)))
}

pub(crate) async fn legal(ex: &(impl Executor + ?Sized), attacker: i64, victim: i64, now: i64) -> Result<bool, CallError> {
    let (Some((attacker, _)), Some((victim, _))) = (affiliation(ex, attacker).await?, affiliation(ex, victim).await?) else { return Ok(false) };
    if attacker == victim { return Ok(false); }
    Ok(!wars::active_by_owners(ex, &[attacker, victim], now).await.map_err(db_err)?.is_empty())
}

pub(crate) async fn index(ex: &(impl Executor + ?Sized), kill_id: i64, victim_corp: i64, victim_alliance: Option<i64>, attackers: &[Affiliation], now: i64) -> Result<(), CallError> {
    let victim_owner = victim_alliance.unwrap_or(victim_corp);
    let owners: HashSet<i64> = attackers.iter().map(|a| a.alliance.unwrap_or(a.corporation)).collect();
    let mut query_owners: Vec<_> = owners.iter().copied().collect();
    query_owners.push(victim_owner);
    for war in wars::active_by_owners(ex, &query_owners, now).await.map_err(db_err)? {
        let opposing = if war.declared_by == victim_owner { Some(war.against) } else if war.against == victim_owner { Some(war.declared_by) } else { None };
        if let Some(attacker_side) = opposing.filter(|owner| *owner != victim_owner && owners.contains(owner)) {
            projections::insert(ex, &projections::Projection { kill_id, war_id: war.war_id, attacker_side, defender_side: victim_owner }).await.map_err(db_err)?;
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
