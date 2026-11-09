use super::*;

#[test]
fn field_volume_sampling_uses_half_open_range() {
    crate::tests::init_config();
    let range = crate::config::game().mining.ore_rock_m3;
    assert_eq!(sample_range(range, 0.0), range[0]);
    assert_eq!(sample_range(range, 0.5), range[0] + (range[1] - range[0]) / 2.0);
    assert_eq!(sample_range([17.0, 17.0], 0.5), 17.0);
}

#[test]
fn ore_bands_follow_security() {
    assert!(field_groups(Field::Ore, 0.9, false).0.contains(&"Veldspar"));
    assert!(field_groups(Field::Ore, 0.3, false).0.contains(&"Jaspet"));
    assert!(field_groups(Field::Ore, -0.4, false).0.contains(&"Arkonor"));
    assert!(field_groups(Field::Gas, -1.0, true).0.iter().all(|g| g.starts_with("Fullerite")));
    assert!(rock_radius(Field::Ore, 25_000.0) < 10_000.0);
}

#[test]
fn ore_candidates_require_matching_group_category_and_exact_grade() {
    let names = ["Veldspar"];
    assert!(ore_candidate("Veldspar", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(ore_candidate("Veldspar II-Grade", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(ore_candidate("Veldspar III-Grade", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(!ore_candidate("Veldspar IV-Grade", "Veldspar", Some(CATEGORY_ASTEROID), &names));
    assert!(!ore_candidate("Veldspar", "Veldspar", None, &names));
    assert!(!ore_candidate("Scordite", "Scordite", Some(CATEGORY_ASTEROID), &names));
}
