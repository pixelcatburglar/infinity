//! Player-kill count read for the client `shipKillCounter` remote service.
//! The count persists in `game.ship_kill_counts` and follows the physical hull
//! across ownership changes; repackaging clears the row. The hangar model
//! clamps the displayed value client-side and nothing on the server attributes
//! kills automatically. A caller reads the real count only for a hull its
//! selected character owns, wherever that hull currently rests, or for a
//! foreign hull the shared configuration visibility check authorizes; every
//! other well-formed query reports zero instead of a refusal. The read never
//! initializes a row and never grants GM reach.
use super::{configuration, Call, CallResult, Client, Value, db_err};
use infinity_core::consts::CATEGORY_SHIP;
use crate::dao::{characters::selection, items::ship_appearance::{self, ObservedHull}};
use crate::services::{notify_error, CallError};
use infinity_core::messages::ship as text;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.method != "GetItemKillCountPlayer" { return None; }
    Some(get(c, call).await)
}

/// GetItemKillCountPlayer(itemID): the persisted player-kill count of one
/// physical hull. Only a malformed argument shape is an error; every
/// unauthorized or unresolvable hull reports `Value::Int(0)`.
async fn get(c: &Client, call: &Call<'_>) -> CallResult {
    let ship = match call.args {
        [Value::Int(id)] if *id > 0 => *id,
        _ => return Err(notify_error(text::HULL_APPEARANCE_INACCESSIBLE)),
    };
    let Some(character) = c.session.int("charid").filter(|id| *id > 0) else { return Ok(Value::Int(0)) };
    if !selection::owns(&c.db, character, c.login.user_id).await.map_err(db_err)? { return Ok(Value::Int(0)); }
    let Some(stored) = ship_appearance::load(&c.db, ship).await.map_err(db_err)? else { return Ok(Value::Int(0)) };
    if c.sde.types.get(stored.hull.type_id).and_then(|ty| ty.category_id) != Some(CATEGORY_SHIP) { return Ok(Value::Int(0)); }
    if stored.hull.owner_id == character { return Ok(Value::Int(stored.player_kills)); }
    // A foreign hull must be locally visible, and the joined custody row must
    // still be the exact item the visibility check authorized.
    match configuration::visible_hull(c, ship, character).await {
        Ok(Some(item)) if stored.hull == ObservedHull::from(&item) => Ok(Value::Int(stored.player_kills)),
        Ok(_) => Ok(Value::Int(0)),
        Err(CallError::User { .. }) => Ok(Value::Int(0)),
        Err(error) => Err(error),
    }
}
