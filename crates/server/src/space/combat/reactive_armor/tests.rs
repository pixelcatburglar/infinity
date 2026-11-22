use super::adapt;
const SEED: f64 = 0.85; const STEP: f64 = 0.06;

fn near<const N: usize>(actual: [f64; N], expected: [f64; N]) {
    assert!(actual.iter().zip(expected).all(|(a, e)| (a - e).abs() < 1e-12), "{actual:?} != {expected:?}");
}

#[test]
fn adapt_donates_saturates_ties_and_keeps_its_pool() {
    let (seed, pool) = ([SEED; 4], 4.0 * SEED);
    near(adapt(seed, [1.0, 0.0, 0.0, 0.0], STEP), [SEED - 3.0 * STEP, SEED + STEP, SEED + STEP, SEED + STEP]);
    let mut profile = seed;
    for _ in 0..3 { profile = adapt(profile, [1.0, 0.0, 0.0, 0.0], STEP); }
    near(profile, [SEED - 0.45, 1.0, 1.0, 1.0]);
    near(adapt(seed, [2.0, 0.0, 0.0, 1.0], STEP), [SEED - STEP, SEED + STEP, SEED + STEP, SEED - STEP]);
    let all = adapt(seed, [1.0; 4], STEP);
    near([all[0], all[3]], [SEED + STEP; 2]);
    near([all[2], all[1]], [SEED - STEP; 2]);
    let paired = adapt(seed, [1.0, 1.0, 2.0, 2.0], STEP);
    near([paired[0], paired[1]], [SEED + STEP; 2]);
    near([paired[2], paired[3]], [SEED - STEP; 2]);
    for damage in [[1.0, 0.0, 0.0, 0.0], [0.0; 4], [2.0, 0.0, 0.0, 1.0], [1.0; 4], [1.0, 1.0, 2.0, 2.0]] {
        assert!((adapt(seed, damage, STEP).iter().sum::<f64>() - pool).abs() < 1e-12);
    }
    assert!((profile.iter().sum::<f64>() - pool).abs() < 1e-12);
    let shifted = adapt(seed, [1.0, 0.0, 0.0, 0.0], STEP);
    near([adapt(shifted, [0.0, 1.0, 0.0, 0.0], STEP)[0]], [shifted[0] + STEP]); near(adapt(shifted, [0.0; 4], STEP), shifted);
}
