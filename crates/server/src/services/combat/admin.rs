//! GM forced loss of the caller's own in-space ship.
//!
//! The `slash` service's `/suicide` command is the only sender. The existing
//! ship-loss service owns every durable write through the shared guarded damage
//! path: the item closure, insurance, killmail, event and the capsule,
//! escape-ship or home replacement. Other targets, manufactured drops and
//! clone-body losses are not supported.

use std::sync::Arc;

use crate::db::Db;
use crate::lua::events::resources::FactOrigin;
use crate::services::identity::Identity;
use crate::services::{notify_error, CallError};
use crate::space::{combat::CombatPhase, System, World};
use infinity_core::{messages, session::OnlineSession};

use super::damage::{apply_damage_guarded, DamageSource};

/// Actor-side admission: the caller's session, piloted ball and persistent
/// combat membership still describe the same live ship.
fn live_member(s: &System, character: i64, ship: i64, expected: &OnlineSession) -> bool {
    s.combat_session_current(character, Some(expected))
        && s.pilot_ball(character)
            .is_some_and(|ball| ball.id == ship && ball.char_id == Some(character) && ball.owner_id == character)
        && s.persistent_generation(ship).is_some()
        && s.persistent_generation(ship) == s.combat_membership_generation(ship)
        && s.combat_phase(ship) == Some(CombatPhase::Live)
}

/// Destroy `identity`'s current in-space ship through the shared guarded loss
/// path. `expected` is the caller's session snapshot taken when the command was
/// issued; the admission capture and the damage guard both compare against it,
/// so a replaced session or ship refuses instead of destroying a new target.
pub(crate) async fn suicide(
    identity: &Identity,
    db: &Db,
    world: &Arc<World>,
    expected: &OnlineSession,
) -> Result<(), CallError> {
    let (character, ship, system) = (identity.char_id, identity.ship_id, identity.system_id);
    if !(character > 0
        && character == expected.character_id
        && ship.is_some_and(|id| id > 0)
        && ship == expected.ship_id
        && system.is_some_and(|id| id > 0)
        && system == expected.system_id
        && identity.station_id.is_none()
        && expected.station_id.is_none())
    {
        return Err(notify_error(messages::gm::GM_SUICIDE_UNAVAILABLE));
    }
    let (ship, system) = (ship.unwrap(), system.unwrap());
    let expected = expected.clone();
    // One actor turn: admit the fixed identity, then initialize the existing
    // facts store and capture the membership generation and incarnation the
    // final guard must still find.
    let Some((expected, generation, incarnation)) = world
        .with(system, move |s| {
            if !live_member(s, character, ship, &expected) {
                return None;
            }
            s.ensure_combat_facts();
            Some((expected, s.combat_membership_generation(ship)?, s.combat_entity(ship)?.incarnation))
        })
        .await
        .flatten()
    else {
        return Err(notify_error(messages::gm::GM_SUICIDE_UNAVAILABLE));
    };
    let guard = move |s: &System| {
        live_member(s, character, ship, &expected)
            && s.combat_membership_generation(ship) == Some(generation)
            && s.combat_entity(ship).is_some_and(|entity| entity.incarnation == incarnation)
    };
    match apply_damage_guarded(
        db,
        world,
        system,
        ship,
        [0.0; 4],
        DamageSource { force_origin: Some(FactOrigin::Gm), ..Default::default() },
        true,
        guard,
    )
    .await?
    {
        Some(outcome) if outcome.lethal => Ok(()),
        _ => Err(notify_error(messages::gm::GM_SUICIDE_UNAVAILABLE)),
    }
}

#[cfg(all(test, feature = "tests-combat"))]
mod tests;
