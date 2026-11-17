//! Disintegrator banks: each member's bound ramp scales its own raw, every
//! really fired member advances once, and an uncharged member never joins.
use super::*;
use crate::services::effects::parameters::MemberInputs;
use crate::services::effects::spool::SpoolParameters;
use infinity_core::consts::dogma::EFFECT_TARGET_DISINTEGRATOR_ATTACK;

fn ramp_of(cycle: &Cycle, member: usize) -> SpoolParameters {
    match &cycle.inputs.members[member].packet { MemberPacket::Turret(inputs) => inputs.spool.unwrap(), _ => panic!("turret packet") }
}
fn ramp_seen(member: &MemberInputs, parameters: SpoolParameters) -> (Option<f64>, Option<f64>) {
    (member.spool_applied, member.spool.map(|state| state.current(parameters)))
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn member_ramps_scale_volleys_and_excluded_members_never_advance() {
    let rig = rig_effect(Some(EFFECT_TARGET_DISINTEGRATOR_ATTACK)).await;

    let mut cycle = rig.arm(2).await;
    let parameters = ramp_of(&cycle, 0);
    assert!(cycle.inputs.members.iter().all(|member| member.spool.is_some()));
    cycle.inputs.members[0].spool.as_mut().unwrap().advance(parameters);
    let (rev0, before) = rig.snap().await;
    let (rolls, _, outcome) = rig.volley(&mut cycle, 1.0).await;
    let (rev1, after) = rig.snap().await;
    let mut raw = [0.0; 4];
    for (index, bonus) in [(0usize, parameters.step), (1, 0.0)] {
        let MemberPacket::Turret(inputs) = &cycle.inputs.members[index].packet else { panic!("turret packet") };
        for (sum, value) in raw.iter_mut().zip(inputs.raw) { *sum += value * (1.0 + bonus); }
    }
    let want: f64 = raw.iter().zip(rig.stats.resonance[0]).map(|(raw, resonance)| raw * resonance).sum();
    let delta: f64 = before.now.iter().zip(after.now).map(|(old, new)| old - new).sum();
    assert!(matches!(outcome, Ok(CycleOutcome::Continue)));
    assert_eq!((rev1 - rev0, rolls), (1, 1));
    assert!((delta - want).abs() <= 1e-6);
    for (index, applied) in [(0usize, parameters.step), (1, 0.0)] {
        assert_eq!(ramp_seen(&cycle.inputs.members[index], parameters), (Some(applied), Some((applied + parameters.step).min(parameters.maximum))));
    }

    let mut cycle = rig.arm(2).await;
    let (rev0, before) = rig.snap().await;
    let (rolls, _, outcome) = rig.volley(&mut cycle, 0.0).await;
    let (rev1, after) = rig.snap().await;
    assert!(matches!(outcome, Ok(CycleOutcome::Continue)));
    assert_eq!((rev1 - rev0, rolls, after.now), (0, 1, before.now));
    assert!(cycle.inputs.members.iter().all(|member| ramp_seen(member, parameters) == (Some(0.0), Some(parameters.step))));

    rig.set_charge(1, rig.ammo2).await;
    let mut cycle = rig.arm(2).await;
    let (rolls, _, outcome) = rig.volley(&mut cycle, 1.0).await;
    assert!(matches!(outcome, Ok(CycleOutcome::Continue)) && rolls == 2);
    assert!(cycle.inputs.members.iter().all(|member| ramp_seen(member, parameters) == (Some(0.0), Some(parameters.step))));

    rig.drop_charge(1).await;
    let mut cycle = rig.arm(2).await;
    let (rolls, _, outcome) = rig.volley(&mut cycle, 1.0).await;
    assert!(matches!(outcome, Ok(CycleOutcome::Continue)) && rolls == 1);
    assert_eq!((ramp_seen(&cycle.inputs.members[0], parameters), ramp_seen(&cycle.inputs.members[1], parameters)),
        ((Some(0.0), Some(parameters.step)), (None, Some(0.0))));
    rig.finish().await;
}
