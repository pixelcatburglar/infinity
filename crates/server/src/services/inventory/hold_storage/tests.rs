//! Synthetic static-data checks for ordinary-container accounting.

use super::*;
use infinity_core::consts::FLAG_DRONE_BAY;
use crate::sde::types::tests::capacity_fixture;

fn row(id: i64, kind: i32, location: i64, quantity: i64) -> GameItem {
    GameItem { item_id: id, type_id: kind, owner_id: id, location_id: location, flag_id: id as i32, quantity, singleton: false, custom_info: None }
}

#[test]
fn static_classification_and_capacity_precedence() {
    let groups: Vec<_> = CARGO_CONTAINER_GROUPS.iter().enumerate().map(|(i, group)| (i as i32 + 2, Some(*group))).collect();
    let first = groups[0].0;
    let sde = capacity_fixture(&groups, &[(first, Some(20.0), Some(1.0))], &[(first, vec![(ATTR_CAPACITY, 7.0)])]);
    for &(id, _) in &groups { assert!(is_ordinary_container(&sde, id)); }
    assert!(!is_ordinary_container(&sde, i32::MAX));
    assert_eq!(static_capacity(&sde, first).unwrap(), 7.0);
    assert_eq!(static_capacity(&sde, groups[1].0).unwrap(), 0.0);
    assert!(static_capacity(&sde, i32::MAX).is_err());
}

#[test]
fn all_direct_rows_count_without_parent_quantity_multiplier() {
    let group = CARGO_CONTAINER_GROUPS[0];
    let sde = capacity_fixture(&[(2, Some(group)), (3, None)], &[(2, Some(10.0), Some(2.0)), (3, None, Some(0.5))], &[]);
    let parent = row(10, 2, 1, 9);
    let rows = [row(11, 3, 10, 2), row(12, 3, 10, 3), row(13, 3, 11, 99)];
    let room = container_room(&sde, &parent, &rows, &[]).unwrap().unwrap();
    assert_eq!((room.capacity, room.used), (CapacityLimit::Finite(10.0), 2.5));
    assert!(container_room(&sde, &row(20, 3, 1, 1), &rows, &[]).unwrap().is_none());
}

#[test]
fn invalid_capacity_and_volume_are_rejected() {
    for value in [-1.0, f64::NAN, f64::INFINITY] {
        let sde = capacity_fixture(&[(2, Some(CARGO_CONTAINER_GROUPS[0])), (3, None)], &[(2, Some(value), Some(1.0)), (3, None, Some(1.0))], &[]);
        assert!(container_room(&sde, &row(10, 2, 1, 1), &[], &[]).is_err());
        let sde = capacity_fixture(&[(2, Some(CARGO_CONTAINER_GROUPS[0])), (3, None)], &[(2, Some(1.0), Some(1.0)), (3, None, Some(value))], &[]);
        assert!(container_room(&sde, &row(10, 2, 1, 1), &[row(11, 3, 10, 1)], &[]).is_err());
    }
    let sde = capacity_fixture(&[(2, Some(CARGO_CONTAINER_GROUPS[0])), (3, None)], &[(2, Some(1.0), Some(1.0))], &[]);
    assert!(container_room(&sde, &row(10, 2, 1, 1), &[row(11, 3, 10, 1)], &[]).is_err());
}

#[test]
fn replacement_rejects_duplicate_and_unknown_rows() {
    let sde = capacity_fixture(&[(2, None)], &[(2, None, Some(1.0))], &[]);
    let rows = [row(10, 2, 1, 3)];
    let additions = [Addition { type_id: 2, singleton: false, quantity: 3 }];
    assert_eq!(replacement_volume(&sde, FLAG_CARGO, &rows, &[], &additions, Replaced { items: &[10], reloads: &[] }).unwrap(), (3.0, 3.0));
    assert!(replacement_volume(&sde, FLAG_CARGO, &rows, &[], &additions, Replaced { items: &[10, 10], reloads: &[] }).is_err());
    assert!(replacement_volume(&sde, FLAG_CARGO, &rows, &[], &additions, Replaced { items: &[11], reloads: &[] }).is_err());
}

#[test]
fn exact_fit_and_smallest_positive_growth_have_no_epsilon() {
    let tiny = f64::from_bits(1);
    let sde = capacity_fixture(&[(2, None)], &[(2, None, Some(tiny))], &[]);
    assert!(holds::check(&sde, FLAG_CARGO, 2, false, 1, CapacityLimit::Finite(tiny), 0.0).is_ok());
    assert!(holds::check(&sde, FLAG_CARGO, 2, false, 1, CapacityLimit::Finite(tiny), tiny).is_err());
    assert!(holds::check(&sde, FLAG_CARGO, 2, false, 1, CapacityLimit::Finite(0.0), 0.0).is_err());
    // Unlimited waives the quota on the cargo flag but still refuses corrupt
    // used/occupancy and is meaningless on special bays.
    assert!(holds::check(&sde, FLAG_CARGO, 2, false, i64::MAX, CapacityLimit::Unlimited, 1e12).is_ok());
    assert!(holds::check(&sde, FLAG_CARGO, 2, false, 1, CapacityLimit::Unlimited, f64::NAN).is_err());
    assert!(holds::check(&sde, FLAG_CARGO, 2, false, 1, CapacityLimit::Unlimited, -1.0).is_err());
    assert!(holds::check(&sde, FLAG_DRONE_BAY, 2, false, 1, CapacityLimit::Unlimited, 0.0).is_err());
    let room = Room { capacity: CapacityLimit::Unlimited, used: 1e12 };
    assert_eq!(room.units(&sde, FLAG_CARGO, 2, false, i64::MAX).unwrap(), i64::MAX);
    assert_eq!(room.units(&sde, FLAG_CARGO, 2, false, 0).unwrap(), 0);
    let finite = Room { capacity: CapacityLimit::Finite(tiny), used: 0.0 };
    let larger = capacity_fixture(&[(6, None)], &[(6, None, Some(1.0))], &[]);
    assert_eq!(finite.units(&larger, FLAG_CARGO, 6, false, 0).unwrap(), 0);
    let invalid = Room { capacity: CapacityLimit::Unlimited, used: f64::INFINITY };
    assert!(invalid.units(&sde, FLAG_CARGO, 2, false, 1).is_err());
    // True zero volume accepts and returns the requested amount unchanged;
    // invalid units and an occupancy that stops being finite both refuse.
    let zero = capacity_fixture(&[(3, None)], &[(3, None, Some(0.0))], &[]);
    assert_eq!(room.units(&zero, FLAG_CARGO, 3, false, i64::MAX).unwrap(), i64::MAX);
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        let sde = capacity_fixture(&[(4, None)], &[(4, None, Some(bad))], &[]);
        assert!(room.units(&sde, FLAG_CARGO, 4, false, 1).is_err());
    }
    let huge = capacity_fixture(&[(5, None)], &[(5, None, Some(f64::MAX / 2.0))], &[]);
    assert!(Room { capacity: CapacityLimit::Unlimited, used: f64::MAX }.units(&huge, FLAG_CARGO, 5, false, 1).is_err());
}

#[test]
fn non_singleton_aggregation_accepts_wide_totals() {
    let sde = capacity_fixture(&[(2, None)], &[(2, None, Some(1.0))], &[]);
    assert_eq!(total_volume(&sde, FLAG_CARGO, [(2, false, 2), (2, false, 3)].into_iter()).unwrap(), 5.0);
    for quantity in [1, i64::MAX] {
        assert_eq!(total_volume(&sde, FLAG_CARGO, [(2, false, i64::MAX), (2, false, quantity)].into_iter()).unwrap(), (i128::from(i64::MAX) + i128::from(quantity)) as f64);
    }
}

#[test]
fn wide_reservation_replacement_requires_full_credit() {
    let sde = capacity_fixture(&[(2, None)], &[(2, None, Some(1.0))], &[]);
    let reservation = |id, quantity| PendingReload { reload_id: id, owner_id: 1, ship_id: 1, module_id: id, flag_id: 1, type_id: 2, quantity, source_flag: FLAG_CARGO, source_location_id: 1 };
    let reloads = [reservation(1, i64::MAX), reservation(2, i64::MAX)];
    let additions = [Addition { type_id: 2, singleton: false, quantity: i64::MAX }, Addition { type_id: 2, singleton: false, quantity: i64::MAX }];
    let replaced = Replaced { items: &[], reloads: &[1, 2] };
    let (removed, added) = replacement_volume(&sde, FLAG_CARGO, &[], &reloads, &additions, replaced).unwrap();
    assert_eq!((removed, added), (2.0 * i64::MAX as f64, 2.0 * i64::MAX as f64));
    assert!(replacement_volume(&sde, FLAG_CARGO, &[], &reloads, &additions[..1], replaced).is_err());
}

#[test]
fn negative_non_singleton_rows_keep_individual_own_volume() {
    let sde = capacity_fixture(&[(2, None)], &[(2, None, Some(1.0))], &[]);
    for (rows, expected) in [
        ([-1, 5], 6.0),
        ([-1, -1], 2.0),
        ([-5, 5], 6.0),
    ] {
        assert_eq!(total_volume(&sde, FLAG_CARGO, rows.into_iter().map(|quantity| (2, false, quantity))).unwrap(), expected);
    }
    assert_eq!(total_volume(&sde, FLAG_CARGO, [(2, false, 0), (2, false, 5)].into_iter()).unwrap(), 5.0);
}

#[test]
fn fallback_capacity_and_individual_singleton_volume() {
    let group = CARGO_CONTAINER_GROUPS[0];
    let wrap = infinity_core::consts::TYPE_PLASTIC_WRAP;
    let sde = capacity_fixture(
        &[(2, Some(group)), (3, Some(group)), (wrap, None)],
        &[(2, Some(12.0), Some(1.0)), (3, Some(0.0), Some(1.0)), (wrap, None, Some(1.0))],
        &[],
    );
    assert_eq!(static_capacity(&sde, 2).unwrap(), 12.0);
    assert_eq!(container_room(&sde, &row(10, 3, 1, 1), &[], &[]).unwrap().unwrap().capacity, CapacityLimit::Finite(0.0));
    assert_eq!(total_volume(&sde, FLAG_CARGO, [(wrap, true, -125), (wrap, true, -250)].into_iter()).unwrap(), 3.75);
}
