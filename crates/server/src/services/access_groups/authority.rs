//! Access-group authority and live affiliation resolution for ownerGroupManager.
//! Uses game access groups and current corporation/alliance membership; structure policies
//! and immediate eviction on affiliation changes are not supported.
use infinity_core::messages;
use crate::{dao, db::executor::Executor, services::{notify_error, CallError}};
use super::{ADMIN, EXCLUDED, MEMBER, NONE, PUBLIC};

pub(crate) async fn membership(ex: &(impl Executor + ?Sized), character: i64, group: i64) -> Result<i32, CallError> {
    if dao::access_groups::group(ex, group).await?.is_none() {
        return Err(notify_error(messages::access_groups::MISSING));
    }
    let corp = dao::corporations::registry::membership(ex, character).await?.corporation_id;
    let alliance = dao::alliances::live_corporation_alliance(ex, corp).await?.flatten();
    let mut entities = vec![character, corp];
    if let Some(id) = alliance { entities.push(id); }
    entities.push(PUBLIC);
    let rows = dao::access_groups::matching_members(ex, group, &entities).await?;
    if rows.iter().any(|r| r.membership_type == EXCLUDED) { return Ok(EXCLUDED); }
    for id in entities {
        if let Some(row) = rows.iter().find(|r| r.member_id == id) {
            return Ok(if id == character { row.membership_type } else if (MEMBER..=ADMIN).contains(&row.membership_type) { MEMBER } else { NONE });
        }
    }
    Ok(NONE)
}
