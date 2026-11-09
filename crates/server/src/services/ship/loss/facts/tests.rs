use super::*;
use crate::lua::events::resources::FactOrigin;

fn entity(item: i64, character: i64) -> CombatEntity {
    CombatEntity { item_id: item, type_id: 1, incarnation: [1; 16], character_id: Some(character), corporation_id: None, alliance_id: None, faction_id: None }
}
fn context(ship: i64) -> FactContext {
    FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(ship), ship_type_id: Some(1), system_id: Some(1), station_id: None, origin: FactOrigin::Natural }
}

#[test]
fn valuation_distinguishes_missing_and_priced_zero() {
    let items = vec![(2, infinity_core::consts::FLAG_CARGO, false, 3), (3, infinity_core::consts::FLAG_FITTING_FIRST, true, 1)];
    let prices = HashMap::from([(1, 2.5), (2, 0.0)]);
    let value = valuation(1, &items, &prices, 9, crate::isk::IskAmount::parse_decimal("1.25").unwrap()).unwrap();
    assert_eq!(value.components[0].category, ValuationCategory::Hull);
    assert_eq!(value.components[1].category, ValuationCategory::Cargo);
    assert_eq!(value.components[1].quantity, 3);
    assert_eq!(value.components[1].unit_price, Some(BinaryAmount::from_f64(0.0).unwrap()));
    assert_eq!(value.components[2].category, ValuationCategory::Fitting);
    assert_eq!(value.components[2].unit_price, None);
    assert_eq!(value.npc_insurance_paid, crate::isk::IskAmount::parse_decimal("1.25").unwrap());
}

#[test]
fn valuation_preserves_exact_posted_insurance_amount() {
    let posted = crate::isk::IskAmount::parse_decimal("123456789012345678901234567890.12").unwrap();
    let value = valuation(1, &vec![], &HashMap::new(), 1, posted).unwrap();
    assert_eq!(value.npc_insurance_paid, posted);
}

#[test]
fn drone_final_blow_and_prior_drone_credit_retain_physical_sources() {
    let mut one = crate::lua::events::combat::amount::ExactBinarySum::default();
    let mut two = one.clone();
    assert!(one.try_add(100.0));
    assert!(two.try_add(2.0));
    let drone = |item| CombatEntity { character_id: None, ..entity(item, 1) };
    let entry = |character, physical, amount, exact_damage| ContributionEntry { attacker_id: character, source_ship_id: Some(character), source_ship_type: Some(1), weapon_type_id: None, damage: amount, last_damage_at: 1, illegal: false, entity: physical, context: context(character), exact_damage };
    let contributions = vec![entry(2, drone(99), 2.0, two), entry(1, drone(98), 100.0, one)];
    let source = DamageSource { facts: Some(crate::space::combat_facts::CombatSource { actual_actor_character: Some(2), entity: drone(99), context: context(2), source_is_npc: false, controller_incarnation: Some([1; 16]), module_id: None, effect_id: None, weapon_type_id: None, activation: [1; 16], sequence: 1 }), ..Default::default() };
    let value = valuation(1, &vec![], &HashMap::new(), 1, crate::isk::IskAmount::ZERO).unwrap();
    let mut outbox = Outbox::<()>::default();
    let event = events(&mut outbox, 10, 11, entity(10, 10), context(10), 1, ConflictKind::Unknown, &source, &contributions, value);
    if let GameEvent::LossSettled { participants, final_blow, .. } = event {
        assert_eq!(participants.iter().map(|p| p.character_id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(!participants[0].final_blow);
        assert!(participants[1].final_blow);
        assert_eq!(final_blow, Some(drone(99)));
        assert_eq!(participants[0].entity, drone(98));
        assert_eq!(participants[1].entity, drone(99));
        for (participant, entry) in participants.into_iter().zip([&contributions[1], &contributions[0]]) {
            let expected_final = entry.attacker_id == 2;
            match credit(10, 11, entity(10, 10), 1, entry, participant) {
                GameEvent::PlayerKillCredited { char_id, attacker, context: actual_context, final_blow, .. } => {
                    assert_eq!(char_id, entry.attacker_id);
                    assert_eq!(attacker, entry.entity);
                    assert_eq!(actual_context, context(entry.attacker_id));
                    assert_eq!(final_blow, expected_final);
                }
                _ => panic!("expected player kill credit"),
            }
        }
    } else { panic!("expected loss"); }
}

#[test]
fn forced_loss_does_not_invent_a_killer() {
    let value = valuation(1, &vec![], &HashMap::new(), 1, crate::isk::IskAmount::ZERO).unwrap();
    let mut outbox = Outbox::<()>::default();
    let source = DamageSource { force_origin: Some(FactOrigin::Gm), ..Default::default() };
    let event = events(&mut outbox, 10, 11, entity(10, 10), context(10), 1, ConflictKind::Unknown, &source, &[], value);
    if let GameEvent::LossSettled { participants, final_blow, .. } = event {
        assert!(participants.is_empty());
        assert_eq!(final_blow, None);
    } else { panic!("expected loss"); }
}

#[test]
fn zero_damage_never_becomes_a_participant() {
    let contribution = ContributionEntry { attacker_id: 2, source_ship_id: Some(2), source_ship_type: Some(1), weapon_type_id: None, damage: 0.0, last_damage_at: 1, illegal: false, entity: entity(2, 2), context: context(2), exact_damage: Default::default() };
    let value = valuation(1, &vec![], &HashMap::new(), 1, crate::isk::IskAmount::ZERO).unwrap();
    let mut outbox = Outbox::<()>::default();
    let event = events(&mut outbox, 10, 11, entity(10, 10), context(10), 1, ConflictKind::Unknown, &DamageSource::default(), &[contribution], value);
    if let GameEvent::LossSettled { participants, .. } = event {
        assert!(participants.is_empty());
    } else { panic!("expected loss"); }
}

#[test]
fn npc_final_blow_retains_physical_entity_without_credit() {
    let mut npc = entity(20, 20);
    npc.character_id = None;
    let source = DamageSource { facts: Some(crate::space::combat_facts::CombatSource {
        actual_actor_character: None, entity: npc, context: context(20), source_is_npc: true,
        controller_incarnation: None, module_id: None, effect_id: None, weapon_type_id: None,
        activation: [1; 16], sequence: 1,
    }), ..Default::default() };
    let value = valuation(1, &vec![], &HashMap::new(), 1, crate::isk::IskAmount::ZERO).unwrap();
    let mut outbox = Outbox::<()>::default();
    let event = events(&mut outbox, 10, 11, entity(10, 10), context(10), 1, ConflictKind::Npc, &source, &[], value);
    if let GameEvent::LossSettled { participants, final_blow, .. } = event {
        assert_eq!(final_blow, Some(npc));
        assert!(participants.is_empty());
    } else { panic!("expected loss"); }
}
