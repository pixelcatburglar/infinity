use super::*;
use infinity_core::consts::FACTION_CALDARI;

const UNKNOWN_FACTION: i64 = 424242;

#[test]
fn adjusted_picks_bonus_by_sign_and_faction() {
    let b = [0.4, 0.8, 1.2];
    assert_eq!(adjusted(5.0, FACTION_CALDARI, b), 5.0 + 5.0 * 0.8 / 10.0);
    assert_eq!(adjusted(-5.0, FACTION_CALDARI, b), -5.0 + 15.0 * 0.4 / 10.0);
    assert_eq!(adjusted(0.0, FACTION_CALDARI, b), 0.8);
    assert_eq!(adjusted(10.0, FACTION_CALDARI, b), 10.0);
    for faction in PIRATE_FACTIONS {
        assert_eq!(adjusted(5.0, faction, b), 5.0 + 5.0 * 1.2 / 10.0);
        assert_eq!(adjusted(-5.0, faction, b), -5.0 + 15.0 * 0.4 / 10.0);
    }
    for faction in STANDING_SKILL_EXEMPT_FACTIONS {
        assert_eq!(adjusted(5.0, faction, b), 5.0);
        assert_eq!(adjusted(-5.0, faction, b), -5.0);
    }
    assert_eq!(adjusted(5.0, UNKNOWN_FACTION, b), 5.0 + 5.0 * 0.8 / 10.0);
}

#[test]
fn best_takes_min_at_rejection_else_max() {
    assert_eq!(best([1.0, -2.0, 3.0]), -2.0);
    assert_eq!(best([1.0, -1.9, 3.0]), 3.0);
    assert_eq!(best([0.0, 0.0, 0.0]), 0.0);
}
