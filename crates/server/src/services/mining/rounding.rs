//! Pure mining yield and randomized rounding from `packages/mining/__init__.py`.
//! Inputs and random draws are supplied by the caller; this module reads no data sources.
//! Rewards, random number generation, persistence and delivery are not modelled here.
//! Attribute 3154 is expressed in percentage points: the waste check uses
//! `waste_probability > randint(0, 100)`, with both endpoints inclusive, as in
//! `mining/__init__.py:54`; the previous comparison with `rand01` made stored
//! nonzero values such as 34 unconditional.
//! Critical and waste asteroid consumption retains the existing server-policy
//! caps at delivery because the client shared formula does not specify allocation.
//! The pure formula does not apply delivery caps.

#[derive(Clone, Copy, Debug)]
pub(crate) struct MiningInputs {
    pub clamp_factor: f64,
    pub volume: f64,
    pub waste_volume_multiplier: f64,
    pub waste_probability: f64,
    pub crit_quantity_multiplier: f64,
    pub crit_probability: f64,
    pub efficiency: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MiningDraws {
    pub waste_check: u32,
    pub normal_round: f64,
    pub waste_round: f64,
    pub crit_check: f64,
    pub crit_round: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MiningResult {
    pub normal_quantity: i64,
    pub wasted_quantity: i64,
    pub critical_hit_quantity: i64,
}

fn randomized_round(value: f64, draw: f64) -> i64 {
    let truncated = value.trunc();
    truncated as i64 + i64::from(value - truncated > draw)
}

pub(crate) fn mining_result(
    inputs: MiningInputs,
    unit_volume: f64,
    asteroid_quantity: i64,
    draws: MiningDraws,
) -> Result<MiningResult, &'static str> {
    if !(0.0..=1.0).contains(&inputs.efficiency) {
        return Err("MiningResult: Efficiency must be between 0 and 1");
    }

    let normal_volume = inputs.volume * inputs.clamp_factor;
    let wasted_volume = if normal_volume != 0.0 && inputs.waste_probability > f64::from(draws.waste_check) {
        normal_volume * inputs.waste_volume_multiplier * inputs.efficiency
    } else {
        0.0
    };
    let normal_quantity = randomized_round(normal_volume / unit_volume * inputs.efficiency, draws.normal_round)
        .max(0)
        .min(asteroid_quantity);
    let wasted_quantity = randomized_round(
        ((asteroid_quantity - normal_quantity) as f64).min(wasted_volume / unit_volume),
        draws.waste_round,
    ).max(0);
    let critical_hit_quantity = if normal_quantity != 0
        && inputs.crit_probability != 0.0
        && inputs.crit_probability > draws.crit_check
    {
        randomized_round(normal_quantity as f64 * inputs.crit_quantity_multiplier, draws.crit_round)
    } else {
        0
    };
    Ok(MiningResult { normal_quantity, wasted_quantity, critical_hit_quantity })
}

#[cfg(test)]
mod tests;
