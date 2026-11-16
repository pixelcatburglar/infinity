//! Skill-adjusted effective standings (no client service of its own; mirrors
//! eve/common/script/util/standingUtil.py GetStandingBonus and
//! ApplyBonusToStanding, as consumed by standingsvc.GetEffectiveStandingWithAgent
//! in eve/client/script/ui/services/standingsvc.py).
//!
//! Raw values come from game.standings through `super::standing`. The Diplomacy,
//! Connections and Criminal Connections bonuses are character attributes the
//! skills raise through dogma, read once from the caller's already-loaded `Fit`.
//! Factions the client marks unaffected by skill bonuses keep their raw value;
//! a negative standing applies the Diplomacy bonus, a non-negative standing
//! towards a pirate faction applies Criminal Connections, and every other
//! standing applies Connections. The adjusted agent/corporation/faction triple
//! then follows the client rule: the best of the three, unless any is -2 or
//! worse.
//!
//! The client's per-owner faction lookup (agent and corporation each resolved
//! to their own faction) is not reproduced: the caller supplies the agent's
//! faction once and it applies to all three standings.

use super::{standing, CallError, STANDING_LIMIT};
use crate::db::executor::Executor;
use crate::services::fit::{Fit, Key};
use infinity_core::consts::{
    FACTION_ANGEL_CARTEL, FACTION_BLOOD_RAIDER_COVENANT, FACTION_DEATHLESS, FACTION_GURISTAS,
    FACTION_SANSHAS_NATION, FACTION_SERPENTIS, STANDING_SKILL_EXEMPT_FACTIONS,
};

/// SDE dgmAttributeTypes `diplomacyBonus`, raised on the character by Diplomacy.
const ATTR_DIPLOMACY_BONUS: i32 = 356;
/// SDE dgmAttributeTypes `connectionsBonus`, raised on the character by Connections.
const ATTR_CONNECTIONS_BONUS: i32 = 360;
/// SDE dgmAttributeTypes `criminalConnectionsBonus`, raised on the character by Criminal Connections.
const ATTR_CRIMINAL_CONNECTIONS_BONUS: i32 = 361;

/// Client `appConst.factionsPirates`.
const PIRATE_FACTIONS: [i64; 6] = [
    FACTION_ANGEL_CARTEL,
    FACTION_GURISTAS,
    FACTION_BLOOD_RAIDER_COVENANT,
    FACTION_SANSHAS_NATION,
    FACTION_SERPENTIS,
    FACTION_DEATHLESS,
];

/// Client `standingsvc.GetEffectiveStandingWithAgent` rejection threshold.
const STANDING_REJECTION: f64 = -2.0;

/// standingsvc.GetEffectiveStandingWithAgent: the best of the agent's, its corporation's
/// and its faction's skill-adjusted standing, unless one of them is -2 or worse.
pub(crate) async fn effective(
    db: &(impl Executor + ?Sized),
    fit: &Fit,
    char_id: i64,
    agent: i64,
    corp: i64,
    faction: i64,
) -> Result<f64, CallError> {
    let bonuses = [
        fit.value(Key::Char, ATTR_DIPLOMACY_BONUS),
        fit.value(Key::Char, ATTR_CONNECTIONS_BONUS),
        fit.value(Key::Char, ATTR_CRIMINAL_CONNECTIONS_BONUS),
    ];
    Ok(best([
        adjusted(standing(db, char_id, faction).await?, faction, bonuses),
        adjusted(standing(db, char_id, corp).await?, faction, bonuses),
        adjusted(standing(db, char_id, agent).await?, faction, bonuses),
    ]))
}

/// standingUtil.GetStandingBonusSkillTypeID + ApplyBonusToStanding: `bonuses` is
/// [diplomacy, connections, criminal connections].
fn adjusted(raw: f64, faction: i64, bonuses: [f64; 3]) -> f64 {
    if STANDING_SKILL_EXEMPT_FACTIONS.contains(&faction) {
        return raw;
    }
    let [diplomacy, connections, criminal] = bonuses;
    let bonus = if raw < 0.0 {
        diplomacy
    } else if PIRATE_FACTIONS.contains(&faction) {
        criminal
    } else {
        connections
    };
    raw + (STANDING_LIMIT - raw) * bonus / STANDING_LIMIT
}

fn best(standings: [f64; 3]) -> f64 {
    let min = standings.iter().copied().fold(f64::INFINITY, f64::min);
    if min <= STANDING_REJECTION {
        min
    } else {
        standings.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
