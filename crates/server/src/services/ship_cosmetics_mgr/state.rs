//! Effective first-party ship SKIN state for the client ship-state messengers.
//! Applications and licenses come from the game DAO; static compatibility comes
//! from the SDE. Region eligibility is enforced on writes, not projections.
//! In-space reads are served by the actor's stored projection; docked reads
//! keep the original reachability plus DAO path.
use std::collections::HashMap;
use infinity_core::consts::CATEGORY_SHIP;
use crate::{dao::{items::loading::ItemQuery, ship_skins}, db::{executor::Executor, Db}, sde::Sde, services::{db_err, identity::Identity, inventory::reachability::{Endpoint, Reachability}, items::load_items, CallError}, space::{ship_cosmetics::{self, PreparedSkin, Ship}, World}};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct State { pub ship_id: i64, pub character_id: i64, pub skin_id: Option<i32> }

/// The one projection rule: effective applications and license deadlines from
/// the game tables, compatibility from the SDE. A deadline whose `+1` tick is
/// not representable is an internal error, so the mutating transaction rolls
/// back instead of silently treating a finite license as permanent.
pub(crate) async fn prepare(ex: &(impl Executor + ?Sized), sde: &Sde, ships: &[Ship], now: i64) -> Result<Vec<PreparedSkin>, CallError> {
    let ids: Vec<_> = ships.iter().filter(|s| !s.is_npc).map(|s| s.ship_id).collect();
    let rows: HashMap<_, _> = if ids.is_empty() { HashMap::new() } else {
        ship_skins::states_for_ships(ex, &ids, now).await.map_err(db_err)?.into_iter().map(|r| (r.ship_item_id, r)).collect()
    };
    ships.iter().map(|ship| {
        if ship.is_npc {
            return Ok(PreparedSkin { fact: ship.clone(), state: State { ship_id: ship.ship_id, character_id: 0, skin_id: None }, expires_at: None });
        }
        let applied = rows.get(&ship.ship_id).filter(|row| row.type_id == ship.type_id).and_then(|row| {
            let skin = row.skin_id?;
            let owner = row.character_id.filter(|&id| id == row.owner_id)?;
            sde.ship_skins.skin(skin).filter(|s| !s.is_structure_skin && s.supports(ship.type_id)).map(|_| (owner, skin, row.expires_at))
        });
        if let Some((_, _, Some(deadline))) = applied {
            deadline.checked_add(1).ok_or_else(|| CallError::Internal(anyhow::anyhow!("ship SKIN deadline not representable")))?;
        }
        Ok(PreparedSkin {
            fact: ship.clone(),
            state: State { ship_id: ship.ship_id, character_id: applied.map_or(ship.character_id.unwrap_or(0), |a| a.0), skin_id: applied.map(|a| a.1) },
            expires_at: applied.and_then(|a| a.2),
        })
    }).collect()
}

fn space_system(identity: &Identity) -> Result<Option<i64>, CallError> {
    if identity.station_id.is_some() { return Ok(None); }
    identity.system_id.map(Some).ok_or_else(|| CallError::Internal(anyhow::anyhow!("ship SKIN query has no location")))
}
pub(crate) async fn all(world: &World, identity: &Identity) -> Result<Vec<State>, CallError> {
    let Some(system) = space_system(identity)? else { return Ok(Vec::new()) };
    ship_cosmetics::visible_states(world, system, identity.char_id).await
}

/// `Get`: in space the actor's stored projection answers, with no database
/// fallback; docked keeps the original reachability plus DAO read.
pub(crate) async fn get(db: &Db, world: &World, identity: &Identity, ship: i64, now: i64) -> Result<Option<State>, CallError> {
    let mut state = match space_system(identity)? {
        Some(system) => ship_cosmetics::projected_state(world, system, identity.char_id, ship).await?,
        None => None,
    };
    if state.is_none() && identity.station_id.is_some() {
        if let Some(row) = load_items(db, world.sde(), ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next() {
            if row.owner_id == identity.char_id && row.category_id == i64::from(CATEGORY_SHIP) && row.singleton && row.quantity == 1 {
                let endpoint = Endpoint { location: row.item_id, flag: row.flag_id };
                let reach = Reachability::capture(identity, db, world, None, &[endpoint]).await?;
                match reach.check(db, world.sde(), endpoint).await {
                    Ok(()) => {
                        let fact = Ship { ship_id: ship, type_id: row.type_id, character_id: (identity.ship_id == Some(ship)).then_some(identity.char_id), is_npc: false };
                        state = prepare(db, world.sde(), &[fact], now).await?.into_iter().next().map(|prepared| prepared.state);
                    }
                    Err(CallError::User { .. }) => {},
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(state)
}
