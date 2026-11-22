//! Committed stance receipts on the actor: the owner gets `OnStanceActive`, the scene slim gains `shipStance`, live damage and projections survive, and a stale generation or redelivered `new_item` emits nothing.
use super::*;
use crate::services::ship_stance::{state::stance_slim_value, StanceTransition};
use marshal::Value;

fn drain<T>(rx: &mut tokio::sync::mpsc::UnboundedReceiver<T>) -> Vec<T> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn stance_patch(sde: Arc<crate::sde::Sde>, before: &Fit) -> CommittedFitting {
    let mut patch = crate::services::fit::attributes::tests::module_fit(sde, 1, 3, false)
        .into_committed_fitting(before);
    patch.set_ship_stance(StanceTransition { old: 1, new: 2, switched_at: 7, new_item: 2 });
    patch
}

fn ship_stance(system: &System) -> Option<Value> {
    system.ball(1).unwrap().slim.iter().find(|(k, _)| *k == "shipStance").map(|(_, v)| v.clone())
}

fn staged() -> (Arc<crate::sde::Sde>, System, tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, u64, Fit) {
    let (sde, registry) = rig();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    let mut system = System::new(1, registry, 0);
    system.add_persistent_ship(hull());
    let generation = system.persistent_generation(1).unwrap();
    let before = Fit::for_space_type(sde.clone(), 1, vec![]);
    install_fit(&mut system, 1, Fit::for_space_type(sde.clone(), 1, vec![]));
    (sde, system, rx, generation, before)
}

#[test]
fn stance_receipt_publishes_owner_event_and_slim_once() {
    use crate::services::effects::{projection::Projection, EffectKind, JammingType};
    let (sde, mut system, mut rx, generation, before) = staged();
    system.insert(Ball { id: 8, char_id: None, ..hull() });
    assert!(system.set_condition_fraction(1, 0.5));
    let damaged = system.ship_condition(1).unwrap().1;
    system.upsert_projection(1, Projection { source_ship: 8, source_module: 9, source_type: 1,
        effect_id: 1, kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX,
        values: vec![], charge_values: vec![], charge_type: None, resistance_attribute: None, scramble: None });
    let expected = Some(stance_slim_value(Some(1), 7, 2));
    assert!(system.publish_fitting(1, 2, generation, stance_patch(sde.clone(), &before), vec![]));
    let emitted = drain(&mut rx);
    let stance = emitted.iter().find(|(name, _)| name == "OnStanceActive").expect("stance event");
    assert_eq!(stance.1[..], [Value::Int(1), Value::Int(2)]);
    // Damage and the live projection survived the same actor-turn patch.
    assert_eq!(system.ship_condition(1).unwrap().1, damaged);
    assert_eq!(system.with_combat_fit(1, |fit, _, _| fit.source_type(Key::Projected(9)).is_some()), Some(true));
    system.tick(1000);
    assert_eq!(ship_stance(&system), expected);
    drain(&mut rx);
    // A redelivered receipt whose new item is already resident emits nothing.
    assert!(system.publish_fitting(1, 2, generation, stance_patch(sde, &before), vec![]));
    assert!(!drain(&mut rx).iter().any(|(name, _)| name == "OnStanceActive"));
    system.tick(1000);
    assert_eq!(ship_stance(&system), expected);
}

#[test]
fn stale_generation_emits_no_stance_event_or_slim() {
    let (sde, mut system, mut rx, generation, before) = staged();
    assert!(!system.publish_fitting(1, 2, generation.wrapping_add(1), stance_patch(sde, &before), vec![]));
    system.tick(1000);
    assert!(rx.try_recv().is_err());
    assert!(system.ball(1).unwrap().slim.is_empty());
}
