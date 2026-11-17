use super::*;

fn identity(seed: i64) -> ProjectileIdentity {
    ProjectileIdentity { owner: seed, corporation: seed + 10,
        alliance: Some(seed + 20), war_faction: Some(seed + 30), fleet: Some(seed + 40) }
}
fn candidate(id: i64, distance: f64, expiry: i64) -> Candidate {
    Candidate { id, surface_distance: distance, expiry_at_ms: expiry, identity: identity(id + 100) }
}

#[test]
fn admission_requires_all_three_content_gates() {
    assert!(is_defender_fit(Some(GROUP_MISSILE_LAUNCHER_DEFENDER), EFFECT_USE_MISSILES, Some(GROUP_DEFENDER_MISSILE),
        std::iter::once(EFFECT_DEFENDER_MISSILE_LAUNCHING)));
    assert!(!is_defender_fit(Some(GROUP_MISSILE_LAUNCHER_DEFENDER), EFFECT_USE_MISSILES, Some(GROUP_DEFENDER_MISSILE),
        std::iter::empty()));
    assert!(!is_defender_fit(Some(1), EFFECT_USE_MISSILES, Some(GROUP_DEFENDER_MISSILE),
        std::iter::once(EFFECT_DEFENDER_MISSILE_LAUNCHING)));
}

#[test]
fn each_friendship_field_rejects_candidate() {
    let source = identity(1);
    for index in 0..5 {
        let mut target = identity(2);
        match index { 0 => target.owner = source.owner, 1 => target.corporation = source.corporation,
            2 => target.alliance = source.alliance, 3 => target.war_faction = source.war_faction,
            _ => target.fleet = source.fleet }
        assert!(choose_candidate(source, [Candidate { identity: target, ..candidate(2, 1.0, 1) }], &HashSet::new()).is_none());
    }
}

#[test]
fn shared_none_memberships_are_not_friendship() {
    let source = ProjectileIdentity { owner: 1, corporation: 2, alliance: None, war_faction: None, fleet: None };
    let target = ProjectileIdentity { owner: 3, corporation: 4, alliance: None, war_faction: None, fleet: None };
    assert_eq!(choose_candidate(source, [Candidate { identity: target, ..candidate(2, 1.0, 1) }],
        &HashSet::new()).map(|c| c.id), Some(2));
}

#[test]
fn ordering_prefers_nearest_then_expiry_then_id() {
    let source = identity(1);
    let first = candidate(8, 10.0, 2);
    let second = candidate(7, 10.0, 1);
    let third = candidate(6, 9.0, 99);
    assert_eq!(choose_candidate(source, [first, second, third], &HashSet::new()).map(|c| c.id), Some(6));
    assert_eq!(choose_candidate(source, [first, second], &HashSet::new()).map(|c| c.id), Some(7));
    let tied = [candidate(8, 10.0, 1), candidate(7, 10.0, 1)];
    assert_eq!(choose_candidate(source, tied, &HashSet::new()).map(|c| c.id), Some(7));
}

#[test]
fn reserved_candidate_is_excluded() {
    let source = identity(1);
    let target = candidate(2, 1.0, 1);
    let reserved = HashSet::from([target.id]);
    assert!(choose_candidate(source, [target], &reserved).is_none());
}

#[test]
fn non_finite_distance_is_refused() {
    let mut target = candidate(2, f64::NAN, 1);
    assert!(choose_candidate(identity(1), [target], &HashSet::new()).is_none());
    target.surface_distance = f64::INFINITY;
    assert!(choose_candidate(identity(1), [target], &HashSet::new()).is_none());
}

#[test]
fn negative_distance_is_refused() {
    assert!(choose_candidate(identity(1), [candidate(2, -1.0, 1)], &HashSet::new()).is_none());
}

#[test]
fn bomb_threatens_only_when_blast_path_reaches() {
    let source = [0.0, 0.0, 0.0];
    // A straight path over the source is inside the blast.
    assert!(bomb_threatens(source, 10.0, [-100.0, 0.0, 0.0], 1.0, [100.0, 0.0, 0.0], 100.0));
    // An offset path beyond blast plus both surface radii never threatens.
    assert!(!bomb_threatens(source, 10.0, [-100.0, 500.0, 0.0], 1.0, [100.0, 500.0, 0.0], 100.0));
    // A stationary bomb (zero-length path) beside the source still threatens.
    assert!(bomb_threatens(source, 0.0, [50.0, 0.0, 0.0], 0.0, [50.0, 0.0, 0.0], 100.0));
    // Non-finite input fails closed.
    assert!(!bomb_threatens([f64::NAN, 0.0, 0.0], 10.0, [-100.0, 0.0, 0.0], 1.0, [100.0, 0.0, 0.0], 100.0));
    assert!(!bomb_threatens(source, 10.0, [-100.0, 0.0, 0.0], 1.0, [100.0, 0.0, 0.0], f64::INFINITY));
}
