use super::*;

fn inputs(volume: f64) -> MiningInputs {
    MiningInputs {
        clamp_factor: 1.0,
        volume,
        waste_volume_multiplier: 0.0,
        waste_probability: 0.0,
        crit_quantity_multiplier: 0.0,
        crit_probability: 0.0,
        efficiency: 1.0,
    }
}

fn draws() -> MiningDraws {
    MiningDraws {
        waste_check: 0,
        normal_round: 0.25,
        waste_round: 0.25,
        crit_check: 0.25,
        crit_round: 0.25,
    }
}

#[test]
fn randomized_rounding_matches_client_boundaries() {
    assert_eq!(randomized_round(0.5, 0.25), 1);
    assert_eq!(randomized_round(0.5, 0.75), 0);
    assert_eq!(randomized_round(1.5, 0.25), 2);
    assert_eq!(randomized_round(0.5, 0.5), 0);
    assert_eq!(randomized_round(2.0, 0.0), 2);
    assert_eq!(randomized_round(-1.5, 0.25), -1);
    assert_eq!(mining_result(inputs(1.5), 1.0, 1, draws()).unwrap().normal_quantity, 1);
    assert_eq!(mining_result(inputs(-1.5), 1.0, 10, draws()).unwrap().normal_quantity, 0);
}

#[test]
fn waste_probability_uses_inclusive_integer_draw_and_strict_comparison() {
    let mut i = inputs(2.0);
    i.waste_volume_multiplier = 1.0;
    i.waste_probability = 100.0;
    let mut d = draws();
    d.waste_check = 99;
    assert_eq!(mining_result(i, 1.0, 10, d).unwrap().wasted_quantity, 2);
    d.waste_check = 100;
    assert_eq!(mining_result(i, 1.0, 10, d).unwrap().wasted_quantity, 0);
    i.waste_probability = 1.0;
    d.waste_check = 0;
    assert_eq!(mining_result(i, 1.0, 10, d).unwrap().wasted_quantity, 2);
    d.waste_check = 1;
    assert_eq!(mining_result(i, 1.0, 10, d).unwrap().wasted_quantity, 0);
}

#[test]
fn normal_cap_precedes_waste_and_critical_quantity() {
    let mut i = inputs(5.0);
    i.waste_probability = 1.0;
    i.waste_volume_multiplier = 1.0;
    i.crit_probability = 1.0;
    i.crit_quantity_multiplier = 0.5;
    assert_eq!(mining_result(i, 1.0, 3, draws()).unwrap(), MiningResult {
        normal_quantity: 3, wasted_quantity: 0, critical_hit_quantity: 2,
    });
    i.volume = 2.0;
    assert_eq!(mining_result(i, 1.0, 3, draws()).unwrap(), MiningResult {
        normal_quantity: 2, wasted_quantity: 1, critical_hit_quantity: 1,
    });
}

#[test]
fn clamp_and_efficiency_apply_before_rounding_and_waste() {
    let mut i = inputs(12.0);
    i.clamp_factor = 0.5;
    i.efficiency = 0.5;
    i.waste_volume_multiplier = 0.5;
    i.waste_probability = 1.0;
    assert_eq!(mining_result(i, 2.0, 10, draws()).unwrap(), MiningResult {
        normal_quantity: 2, wasted_quantity: 1, critical_hit_quantity: 0,
    });
    i.volume = 1e308;
    i.clamp_factor = 1.0;
    i.efficiency = 0.5;
    i.waste_probability = 0.0;
    assert_eq!(mining_result(i, 1e308, 10, draws()).unwrap().normal_quantity, 1);
}

#[test]
fn critical_requires_nonzero_normal_and_strict_probability() {
    let mut i = inputs(0.0);
    i.crit_probability = 1.0;
    i.crit_quantity_multiplier = 2.0;
    assert_eq!(mining_result(i, 1.0, 10, draws()).unwrap().critical_hit_quantity, 0);
    i.volume = 2.0;
    i.crit_probability = 0.25;
    assert_eq!(mining_result(i, 1.0, 10, draws()).unwrap().critical_hit_quantity, 0);
    i.crit_probability = 0.5;
    i.crit_quantity_multiplier = 0.75;
    assert_eq!(mining_result(i, 1.0, 10, draws()).unwrap().critical_hit_quantity, 2);
}

#[test]
fn efficiency_rejects_out_of_range_and_nan() {
    for efficiency in [-0.01, 1.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut i = inputs(1.0);
        i.efficiency = efficiency;
        assert!(mining_result(i, 1.0, 10, draws()).is_err());
    }
    for efficiency in [0.0, 1.0] {
        let mut i = inputs(1.0);
        i.efficiency = efficiency;
        assert!(mining_result(i, 1.0, 10, draws()).is_ok());
    }
}

#[test]
fn supplied_draws_make_repeated_results_identical() {
    let mut i = inputs(1.5);
    i.waste_probability = 100.0;
    i.waste_volume_multiplier = 0.5;
    i.crit_probability = 0.5;
    i.crit_quantity_multiplier = 0.5;
    assert_eq!(mining_result(i, 1.0, 10, draws()), mining_result(i, 1.0, 10, draws()));
}
