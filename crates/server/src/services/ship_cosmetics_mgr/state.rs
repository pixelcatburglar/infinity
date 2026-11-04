//! Effective first-party ship SKIN state for the client ship-state messengers.
//! Applications and licenses come from the game DAO; static compatibility comes
//! from the SDE. Region eligibility is enforced on writes, not projections.
use std::collections::HashMap;
use crate::{consts::CATEGORY_SHIP, dao::{items::loading::ItemQuery, ship_skins}, db::{executor::Executor, Db}, sde::Sde, services::{db_err, identity::Identity, inventory::reachability::{Endpoint, Reachability}, items::load_items, CallError}, space::{ship_cosmetics::{self, Ship}, World}};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct State { pub ship_id: i64, pub character_id: i64, pub skin_id: Option<i32> }

pub(crate) async fn project(ex: &(impl Executor + ?Sized), sde: &Sde, ships: &[Ship], now: i64) -> Result<Vec<State>, CallError> {
    let ids: Vec<_> = ships.iter().filter(|s| !s.is_npc).map(|s| s.ship_id).collect();
    let rows: HashMap<_, _> = if ids.is_empty() { HashMap::new() } else {
        ship_skins::states_for_ships(ex, &ids, now).await.map_err(db_err)?.into_iter().map(|r| (r.ship_item_id, r)).collect()
    };
    Ok(ships.iter().map(|ship| {
        if ship.is_npc { return State { ship_id: ship.ship_id, character_id: 0, skin_id: None }; }
        let applied = rows.get(&ship.ship_id).filter(|row| row.type_id == ship.type_id).and_then(|row| {
            let skin = row.skin_id?;
            let owner = row.character_id.filter(|&id| id == row.owner_id)?;
            sde.ship_skins.skin(skin).filter(|s| !s.is_structure_skin && s.supports(ship.type_id)).map(|_| (owner, skin))
        });
        State { ship_id: ship.ship_id, character_id: applied.map_or(ship.character_id.unwrap_or(0), |a| a.0), skin_id: applied.map(|a| a.1) }
    }).collect())
}

pub(crate) async fn all(db: &Db, world: &World, sde: &Sde, identity: &Identity, now: i64) -> Result<Vec<State>, CallError> {
    project(db, sde, &ship_cosmetics::visible(world, identity).await?, now).await
}

pub(crate) async fn get(db: &Db, world: &World, identity: &Identity, ship: i64, now: i64) -> Result<Option<State>, CallError> {
    let mut fact = ship_cosmetics::get_visible(world, identity, ship).await?;
    if fact.is_none() && identity.station_id.is_some() {
        if let Some(row) = load_items(db, world.sde(), ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next() {
            if row.owner_id == identity.char_id && row.category_id == i64::from(CATEGORY_SHIP) && row.singleton && row.quantity == 1 {
                let endpoint = Endpoint { location: row.item_id, flag: row.flag_id };
                let reach = Reachability::capture(identity, db, world, None, &[endpoint]).await?;
                match reach.check(db, world.sde(), endpoint).await {
                    Ok(()) => fact = Some(Ship { ship_id: ship, type_id: row.type_id, character_id: (identity.ship_id == Some(ship)).then_some(identity.char_id), is_npc: false }),
                    Err(CallError::User { .. }) => {},
                    Err(error) => return Err(error),
                }
            }
        }
    }
    let Some(fact) = fact else { return Ok(None) };
    Ok(project(db, world.sde(), &[fact], now).await?.into_iter().next())
}
