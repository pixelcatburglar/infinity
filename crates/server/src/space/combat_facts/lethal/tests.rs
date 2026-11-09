use super::*;
use crate::lua::events::{combat::CombatEntity, resources::{FactContext, FactOrigin}};
use crate::space::combat_facts::CombatSource;
use crate::lua::events::combat::amount::ExactBinarySum;

fn entity(item_id: i64) -> CombatEntity {
    CombatEntity { item_id, type_id: 1, incarnation: [item_id as u8; 16], character_id: None,
        corporation_id: None, alliance_id: None, faction_id: None }
}
fn pending() -> PendingDamage {
    let source = CombatSource { actual_actor_character: None, entity: entity(1), source_is_npc: true,
        controller_incarnation: None, context: FactContext { corporation_id: None, alliance_id: None,
            faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(7), station_id: None,
            origin: FactOrigin::Natural }, module_id: None, effect_id: None, weapon_type_id: None,
        activation: [4; 16], sequence: 9 };
    let mut amounts = std::array::from_fn(|_| ExactBinarySum::default());
    assert!(amounts[0].try_add(1.0));
    assert!(amounts[2].try_add(2.0));
    PendingDamage { settlement: SettlementId { source_instance: source.activation, generation: 4, sequence: 9,
        target_id: 2, target_incarnation: [2; 16] }, source, target: entity(2), occurred_at: 10,
        amounts, slice: 3, qualification_epoch: 5, boundary_at: 10 }
}

#[test]
fn lethal_layers_are_frozen_and_retry_stable() {
    let damage = pending();
    let mut first: Outbox = Outbox::default();
    let mut second: Outbox = Outbox::default();
    damage.append(&mut first);
    damage.append(&mut second);
    let (_, first) = crate::db::outbox::tests::split(first);
    let (_, second) = crate::db::outbox::tests::split(second);
    assert_eq!(serde_json::to_value(&first).unwrap(), serde_json::to_value(&second).unwrap());
    assert_eq!(first.len(), 2);
    for event in first {
        let GameEvent::DamageApplied { char_id, context, slices, .. } = event else { panic!("damage event expected") };
        assert_eq!(char_id, None);
        assert_eq!(context.system_id, Some(7));
        assert_eq!(slices[0].qualification_epoch, 5);
        assert_eq!(slices[0].boundary_at, 10);
        assert_eq!(slices[0].first_occurred_at, 10);
        assert_eq!(slices[0].active_until, 10);
    }
    let id = LethalIdentity { settlement: damage.settlement, layer: 2 };
    assert_eq!(SourceKey::canonical("lethal", &id, 2).as_str(), SourceKey::canonical("lethal", &id, 2).as_str());
    assert_ne!(SourceKey::canonical("lethal", &id, 2).as_str(), SourceKey::canonical("lethal", LethalIdentity { settlement: damage.settlement, layer: 0 }, 0).as_str());
}
