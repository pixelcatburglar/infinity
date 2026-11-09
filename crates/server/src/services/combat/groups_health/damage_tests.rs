use super::set_damage_layers;

#[test]
fn selected_layers_change_without_healing_other_damage() {
    let max = [100.0, 200.0, 300.0];
    let now = [80.0, 100.0, 150.0];
    assert_eq!(set_damage_layers(now, max, max, [true, false, false], 0.25), [75.0, 100.0, 150.0]);
    assert_eq!(set_damage_layers(now, max, max, [true; 3], 0.5), [50.0, 100.0, 150.0]);
    assert_eq!(set_damage_layers(now, max, max, [false, true, false], 1.0), [80.0, 0.0, 150.0]);
}

#[test]
fn unselected_damage_survives_maximum_changes() {
    let old_max = [100.0, 200.0, 300.0];
    let now = [80.0, 100.0, 150.0];
    assert_eq!(set_damage_layers(now, old_max, [120.0, 400.0, 200.0], [true, false, false], 0.25), [90.0, 300.0, 50.0]);
    assert_eq!(set_damage_layers(now, old_max, [50.0, 50.0, 100.0], [false; 3], 0.0), [50.0, -50.0, -50.0]);
}
