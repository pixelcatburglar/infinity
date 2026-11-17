use super::*;
use crate::space::system::gm_effects::{find, CATALOGUE, Error, Outcome, Request, VisualEffect};
fn args(event: Value) -> Vec<Value> {
    let Value::Tuple(parts) = event else { panic!("event") };
    let Value::Tuple(call) = parts.into_iter().nth(1).unwrap() else { panic!("call") };
    let Value::Tuple(args) = call.into_iter().nth(1).unwrap() else { panic!("args") };
    args
}
fn sent(rx: &mut mpsc::UnboundedReceiver<crate::space::SceneDelivery>) -> Option<Vec<Vec<Value>>> {
    loop {
        match rx.try_recv().ok()? {
            crate::space::SceneDelivery::Destiny(batch) => return Some(batch.into_iter().filter(|e| name(e) == b"OnSpecialFX").map(args).collect()),
            crate::space::SceneDelivery::Notify { event, .. } => assert_eq!(event, "OnFleetStateChange"),
        }
    }
}
fn scene() -> (Arc<crate::sde::Sde>, System, mpsc::UnboundedReceiver<crate::space::SceneDelivery>) {
    let mut s = System::new(0, Arc::default(), 12_345);
    s.insert(ball(1, Some(1), 0.0));
    let rx = watch(&mut s, 7, 1);
    (crate::sde::types::tests::combat_fixture(), s, rx)
}
#[test]
fn preview_follows_existing_pending_events() {
    let (sde, mut s, mut rx) = scene();
    s.begin_destiny_batch();
    s.emit_destiny(crate::space::system::observers::DestinyAudience::System, vec![event(1, "Stop", vec![Value::Int(1)])]);
    assert!(s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::JumpIn, target: None, module_type: None }, &sde).is_ok());
    assert!(rx.try_recv().is_err()); s.finish_destiny_batch();
    assert_eq!(super::events::destiny(rx.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(), vec![b"Stop".as_slice(), b"OnSpecialFX".as_slice()]);
}
#[test]
fn previews_reach_only_the_owning_observer_and_follow_membership() {
    let (sde, mut s, mut rx) = scene();
    s.insert(ball(2, None, 10.0));
    let before = crate::client::filetime_now();
    let mut other = watch(&mut s, 8, 1); // a second observer of the same ego
    let actual = super::events::destiny(rx.try_recv().unwrap());
    let mut expected = vec![crate::space::add_balls_event(s.current_time(), s.ball(2).unwrap(), crate::space::collision::enabled())];
    super::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected);
    assert_eq!(super::events::fleet_snapshot(&mut rx), Some(vec![crate::services::keyval([("targetTags", Value::Dict(vec![]))])]));
    s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::JumpDriveIn, target: None, module_type: None }, &sde).unwrap();
    assert_eq!(sent(&mut rx).unwrap()[0][..8], [Value::Int(1), Value::None, Value::None, Value::None, Value::None,
        Value::str("effects.JumpDriveIn"), Value::Bool(false), Value::Int(1)]); // self start, anonymous module
    s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::RemoteArmourRepair, target: Some(2), module_type: None }, &sde).unwrap();
    assert_eq!(sent(&mut rx).unwrap()[0][3], Value::Int(2)); // targeted preview carries the real ball
    s.insert(ball(3, None, 20.0));
    let replay = s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::RemoteArmourRepair, target: Some(3), module_type: None }, &sde);
    assert!(matches!(replay, Ok(Outcome::Sent { replaced: true, .. })));
    assert_eq!(sent(&mut rx).unwrap().iter().map(|a| (a[3].clone(), a[7].clone())).collect::<Vec<_>>(),
        vec![(Value::Int(2), Value::Int(0)), (Value::Int(3), Value::Int(1))]); // old target stop, then new start
    s.take(3); // removing the target stops only the handle that referenced it
    assert!(matches!(&sent(&mut rx).unwrap()[..], [stop] if stop[3] == Value::Int(3) && stop[7] == Value::Int(0)));
    s.gm_effect(1, 7, 1, Request::Stop { effect: VisualEffect::JumpDriveIn, target: None }, &sde).unwrap();
    assert_eq!(sent(&mut rx).unwrap()[0][7], Value::Int(0));
    assert!(matches!(s.gm_effect(1, 7, 1, Request::StopAll, &sde), Ok(Outcome::Stopped { count: 0 })));
    assert!(s.gm_previews.is_empty() && sent(&mut rx).is_none() && sent(&mut other).is_none()); // nothing leaked
}
#[test]
fn previews_refuse_mismatched_or_ineligible_calls_without_sends() {
    let (sde, mut s, mut rx) = scene();
    assert!(matches!(s.gm_effect(1, 7, 9, Request::Play { effect: VisualEffect::JumpIn, target: None, module_type: None }, &sde), Err(Error::CallerChanged)));
    assert!(matches!(s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::RemoteArmourRepair, target: Some(9), module_type: None }, &sde), Err(Error::InvalidTarget))
        && s.gm_previews.is_empty() && sent(&mut rx).is_none());
}
#[test]
fn stop_all_belongs_to_the_recording_observer_only() {
    let (sde, mut s, _rx) = scene();
    let _other = watch(&mut s, 8, 1); // same ego, a different observer
    s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::JumpIn, target: None, module_type: None }, &sde).unwrap();
    assert!(matches!(s.gm_effect(1, 8, 1, Request::StopAll, &sde), Err(Error::CallerChanged)) && !s.gm_previews.is_empty());
}
#[test]
fn pulses_record_nothing_and_celestial_stops_send_float_zero() {
    let (sde, mut s, mut rx) = scene();
    s.insert(ball(2, None, 10.0));
    s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::MicroJump, target: None, module_type: None }, &sde).unwrap();
    assert!(s.gm_previews.is_empty() && sent(&mut rx).unwrap()[0][7] == Value::Int(1));
    assert!(matches!(s.gm_effect(1, 7, 1, Request::Stop { effect: VisualEffect::MicroJump, target: None }, &sde), Err(Error::OneShotCannotStop)));
    s.gm_effect(1, 7, 1, Request::Play { effect: VisualEffect::CelestialBeam, target: Some(2), module_type: None }, &sde).unwrap();
    assert_eq!(sent(&mut rx).unwrap()[0][13], Value::dict([("isFiring", Value::Float(1.0))]));
    assert!(matches!(s.gm_effect(1, 7, 1, Request::StopAll, &sde), Ok(Outcome::Stopped { count: 1 })));
    let [stop] = sent(&mut rx).unwrap().try_into().unwrap(); // the pulse produced no stop
    assert!(stop[1] == Value::None && stop[13] == Value::dict([("isFiring", Value::Float(0.0))]));
}
#[test]
fn catalogue_rows_resolve_their_names_guid_and_aliases() {
    assert_eq!(CATALOGUE.len(), 32);
    for (names, ok) in [
        ("microjump jump_in jump_drive_in abyssal_reentry_splash jump_drive_in_bo jump_portal jump_portal_bo capsule_flare whiteout", true),
        ("celestial_beam remote_armour_repair shield_transfer data_link main_bank_hacking_beam reserve_bank_hacking_beam turbolaser", true),
        ("lance_amarr lance_caldari lance_gallente lance_minmatar superweapon_amarr superweapon_caldari superweapon_gallente superweapon_minmatar", true),
        ("cloak cloak_no_anim cloak_regardless uncloak triage jump_out_abyssal jump_out_abyssal_between linked_to_trace_gate", false)] {
        for name in names.split_whitespace() {
            let entry = find(name).unwrap();
            assert!(entry.disposition.is_ok() == ok && entry.name == name, "{name}");
            assert!(std::ptr::eq(find(entry.guid).unwrap(), entry)
                && entry.aliases.iter().all(|a| std::ptr::eq(find(a).unwrap(), entry)), "{name}");
        }
    }
    for (spelling, name) in [("effects.CLOAK", "cloak"), ("CloakNoAmim", "cloak_no_anim"), ("super-weapon-minmatar", "superweapon_minmatar")] {
        assert_eq!(find(spelling).unwrap().name, name, "{spelling}");
    }
}
