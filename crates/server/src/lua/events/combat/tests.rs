use super::*;
use crate::lua::events::resources::{FactContext, FactOrigin};

fn entity(id: i64) -> CombatEntity { CombatEntity { item_id: id, type_id: 1, incarnation: [id as u8; 16], character_id: Some(id), corporation_id: None, alliance_id: None, faction_id: None } }
fn context() -> FactContext { FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(1), station_id: None, origin: FactOrigin::Natural } }
fn amount(n: &str) -> BinaryAmount { BinaryAmount { coefficient: n.into(), exponent: 0 } }
fn slice(index: u64, epoch: u64) -> CombatSlice { CombatSlice { index, qualification_epoch: epoch, boundary_at: 10, active_until: 10, first_occurred_at: 10, last_occurred_at: 10, successful_count: 1, amount: amount("1") } }
fn damage(slices: Vec<CombatSlice>) -> GameEvent { GameEvent::DamageApplied { char_id: Some(1), context: context(), source: entity(1), target: entity(2), module_id: Some(3), weapon_type_id: Some(4), effect_id: Some(5), layer: HpLayer::Shield, slices } }
#[test]
fn canonical_binary_amounts_reject_invalid_encodings() {
    for value in [BinaryAmount { coefficient: "00".into(), exponent: 0 }, amount("2"), BinaryAmount { coefficient: "0".into(), exponent: 1 }, amount(&"9".repeat(652))] { assert!(value.validate().is_err()); }
    assert!(amount("0").validate().is_ok());
    assert_eq!(BinaryAmount::from_f64(0.0).unwrap(), amount("0"));
    assert_eq!(BinaryAmount::from_f64(0.5).unwrap(), BinaryAmount { coefficient: "1".into(), exponent: -1 });
    assert!(BinaryAmount::from_f64(f64::NAN).is_err());
}
#[test]
fn equal_timestamp_epochs_and_overlap_validation() {
    let event = damage(vec![slice(0, 0), slice(1, 1)]);
    assert!(validate(&event).is_ok());
    let mut invalid = slice(1, 1); invalid.boundary_at = 9;
    assert!(validate(&damage(vec![slice(0, 0), invalid])).is_err());
    let mut invalid = slice(0, 0); invalid.amount = amount("0");
    assert!(validate(&damage(vec![invalid])).is_err());
}
#[test]
fn lua_preserves_incarnations_and_amount_text() {
    let lua = Lua::new(); let table = damage(vec![slice(0, 0)]).to_lua(&lua).unwrap();
    let source: Table = table.get("source").unwrap(); let id: Table = source.get("incarnation").unwrap();
    assert_eq!(id.raw_len(), 16); assert_eq!(id.get::<u8>(16).unwrap(), 1);
    let slices: Table = table.get("slices").unwrap(); let row: Table = slices.get(1).unwrap(); let value: Table = row.get("amount").unwrap();
    assert_eq!(value.get::<String>("coefficient").unwrap(), "1");
}
fn events() -> Vec<GameEvent> {
    let s = entity(1); let t = entity(2); let ctx = context();
    let valuation = LossValuation { components: vec![ValuationComponent { category: ValuationCategory::Hull, type_id: 1, quantity: 1, unit_price: Some(amount("0")) }], source: ValuationSource::RetailPricesAverage, source_revision: None, valuation_observed_at: 10, npc_insurance_paid: crate::isk::IskAmount::ZERO };
    let key = SettlementKey { settlement: SettlementId { source_instance: [1; 16], generation: 1, sequence: 1, target_id: 2, target_incarnation: [2; 16] }, part: SettlementPart::Whole };
    vec![damage(vec![slice(0, 0), slice(1, 1)]),
        GameEvent::RemoteRepairApplied { char_id: Some(1), context: ctx, source: s, target: t, module_id: None, effect_id: Some(4), layer: HpLayer::Hull, slices: vec![slice(0, 0)] },
        GameEvent::WarpScrambleApplied { char_id: Some(1), context: ctx, source: s, target: t, module_id: Some(3), effect_id: None, slices: vec![slice(0, 0)], intervals: vec![CombatInterval { activation: key, segment_sequence: 0, active_after: 10, active_until: 11 }] },
        GameEvent::NpcDestroyed { death_id: "death".into(), spawn_generation: 1, npc: t, system_id: 1, killer_id: None, killer: None, context: ctx, occurred_at: 10, base_bounty: amount("0"), static_version: None },
        GameEvent::LossSettled { char_id: 2, loss_id: 1, killmail_id: 1, victim: t, context: ctx.clone(), occurred_at: 10, final_blow: None, participants: vec![], conflict: ConflictKind::Unknown, valuation },
        GameEvent::PlayerKillCredited { char_id: 1, loss_id: 1, killmail_id: 1, attacker: s, victim: t, final_blow: true, damage: amount("1"), context: ctx, occurred_at: 10 }]
}
#[test]
fn all_combat_variants_codec_round_trip_and_zero_price() {
    for event in events() { let payload = crate::progress::codec::encode(&event).unwrap(); assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, event.kind(), &payload).unwrap(), event); }
}

fn rejects(event: &GameEvent) {
    assert!(crate::progress::codec::encode(event).is_err());
    let payload = serde_json::to_string(event).unwrap();
    assert!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, event.kind(), &payload).is_err());
}

#[test]
fn insurance_payment_is_exact_isk_and_lua_number() {
    use crate::isk::IskAmount;
    for text in ["0.00", "0.01", "9999999999999999999999999999999999.99"] {
        let paid = IskAmount::parse_decimal(text).unwrap();
        let mut event = events().remove(4);
        if let GameEvent::LossSettled { valuation, .. } = &mut event { valuation.npc_insurance_paid = paid; }
        let payload = crate::progress::codec::encode(&event).unwrap();
        assert!(payload.contains(&format!("\"npc_insurance_paid\":\"{text}\"")));
        assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, event.kind(), &payload).unwrap(), event);
        let lua = Lua::new();
        let valuation: Table = event.to_lua(&lua).unwrap().get("valuation").unwrap();
        assert!(matches!(valuation.get::<Value>("npc_insurance_paid").unwrap(), Value::Number(value) if value == paid.wire_isk()));
    }
    let mut negative = events().remove(4);
    if let GameEvent::LossSettled { valuation, .. } = &mut negative { valuation.npc_insurance_paid = IskAmount::from_cents(-1).unwrap(); }
    rejects(&negative);
}

#[test]
fn scramble_duration_only_boundary_and_empty_event_rules() {
    let mut duration = events().remove(2);
    if let GameEvent::WarpScrambleApplied { slices, intervals, .. } = &mut duration {
        slices.clear();
        intervals[0].active_after = 20;
        intervals[0].active_until = 30;
        intervals[0].segment_sequence = 1;
    }
    // The continuation starts at the boundary without inventing a new cycle.
    let mut prior = events().remove(2);
    if let GameEvent::WarpScrambleApplied { intervals, .. } = &mut prior { intervals[0].active_until = 20; }
    let prior_payload = crate::progress::codec::encode(&prior).unwrap();
    assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, prior.kind(), &prior_payload).unwrap(), prior);
    let encoded = crate::progress::codec::encode(&duration).unwrap();
    assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, duration.kind(), &encoded).unwrap(), duration);
    assert_eq!(metadata(&duration).unwrap().0, 20);
    let mut mixed = duration.clone();
    if let GameEvent::WarpScrambleApplied { slices, .. } = &mut mixed { slices.push(slice(0, 1)); }
    assert_eq!(metadata(&mixed).unwrap().0, 10);
    if let GameEvent::WarpScrambleApplied { intervals, .. } = &mut mixed { intervals[0].active_after = 5; }
    assert_eq!(metadata(&mixed).unwrap().0, 5);
    if let GameEvent::WarpScrambleApplied { intervals, .. } = &mut duration { intervals.clear(); }
    rejects(&duration);
    for mut event in events().into_iter().take(2) {
        match &mut event {
            GameEvent::DamageApplied { slices, .. } | GameEvent::RemoteRepairApplied { slices, .. } => slices.clear(),
            _ => unreachable!(),
        }
        rejects(&event);
    }
}

#[test]
fn malformed_every_variant_is_rejected_by_both_codec_directions() {
    for mut event in events() {
        match &mut event {
            GameEvent::DamageApplied { char_id, .. } => *char_id = Some(0),
            GameEvent::RemoteRepairApplied { module_id, .. } => *module_id = Some(0),
            GameEvent::WarpScrambleApplied { effect_id, .. } => *effect_id = Some(0),
            GameEvent::NpcDestroyed { base_bounty, .. } => *base_bounty = amount("2"),
            GameEvent::LossSettled { victim, .. } => victim.character_id = Some(7),
            GameEvent::PlayerKillCredited { damage, .. } => *damage = amount("0"),
            _ => unreachable!(),
        }
        rejects(&event);
    }
}

#[test]
fn npc_source_has_no_subject_and_lua_keeps_u64_strings() {
    let mut event = damage(vec![slice(u64::MAX, u64::MAX)]);
    if let GameEvent::DamageApplied { char_id, source, module_id, effect_id, slices, .. } = &mut event {
        *char_id = None; source.character_id = None; *module_id = None; *effect_id = None;
        slices[0].successful_count = u64::MAX;
    }
    let payload = crate::progress::codec::encode(&event).unwrap();
    assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, event.kind(), &payload).unwrap(), event);
    assert_eq!(subject(&event), None);
    assert_eq!(metadata(&event).unwrap().0, 10);
    let lua = Lua::new(); let table = event.to_lua(&lua).unwrap();
    assert!(matches!(table.get::<Value>("char_id").unwrap(), Value::Nil));
    let row: Table = table.get::<Table>("slices").unwrap().get(1).unwrap();
    for field in ["index", "qualification_epoch", "successful_count"] {
        assert_eq!(row.get::<String>(field).unwrap(), u64::MAX.to_string());
    }
    let small = damage(vec![slice(0, 0)]).to_lua(&lua).unwrap();
    let row: Table = small.get::<Table>("slices").unwrap().get(1).unwrap();
    assert!(matches!(row.get::<Value>("index").unwrap(), Value::String(_)));
}

#[test]
fn drone_credit_duplicates_and_final_blow_consistency() {
    let mut event = events().remove(4);
    let mut drone = entity(3); drone.character_id = None;
    let participant = KillParticipant { character_id: 1, entity: drone, final_blow: true, damage: amount("1") };
    if let GameEvent::LossSettled { final_blow, participants, .. } = &mut event {
        *final_blow = Some(drone); participants.push(participant.clone());
    }
    assert!(validate(&event).is_ok());
    let mut duplicate = event.clone();
    if let GameEvent::LossSettled { participants, .. } = &mut duplicate { participants.push(participant.clone()); }
    rejects(&duplicate);
    let mut unflagged = event.clone();
    if let GameEvent::LossSettled { participants, .. } = &mut unflagged { participants[0].final_blow = false; }
    rejects(&unflagged);
    let mut mismatch = event;
    if let GameEvent::LossSettled { participants, .. } = &mut mismatch { participants[0].entity.character_id = Some(9); }
    rejects(&mismatch);
}

#[test]
fn npc_authority_and_other_unsigned_fields() {
    let lua = Lua::new();
    let mut npc = events().remove(3);
    if let GameEvent::NpcDestroyed { spawn_generation, .. } = &mut npc { *spawn_generation = u64::MAX; }
    assert_eq!(npc.to_lua(&lua).unwrap().get::<String>("spawn_generation").unwrap(), u64::MAX.to_string());
    let mut wrong_system = npc.clone();
    if let GameEvent::NpcDestroyed { context, .. } = &mut wrong_system { context.system_id = Some(2); }
    rejects(&wrong_system);
    let mut missing_killer = npc.clone();
    if let GameEvent::NpcDestroyed { killer_id, .. } = &mut missing_killer { *killer_id = Some(1); }
    rejects(&missing_killer);
    let mut bad_version = npc;
    if let GameEvent::NpcDestroyed { static_version, .. } = &mut bad_version { *static_version = Some(String::new()); }
    rejects(&bad_version);

    let mut warp = events().remove(2);
    if let GameEvent::WarpScrambleApplied { intervals, .. } = &mut warp {
        intervals[0].segment_sequence = u64::MAX;
        intervals[0].activation.settlement.generation = u64::MAX;
        intervals[0].activation.settlement.sequence = u64::MAX;
    }
    let interval: Table = warp.to_lua(&lua).unwrap().get::<Table>("intervals").unwrap().get(1).unwrap();
    assert_eq!(interval.get::<String>("segment_sequence").unwrap(), u64::MAX.to_string());
    let key: Table = interval.get("activation").unwrap();
    let settlement: Table = key.get("settlement").unwrap();
    for field in ["generation", "sequence"] { assert_eq!(settlement.get::<String>(field).unwrap(), u64::MAX.to_string()); }
    assert!(matches!(settlement.get::<Table>("source_instance").unwrap().get::<Value>(1).unwrap(), Value::Integer(_)));
}

#[test]
fn contradictory_source_and_final_blow_are_rejected() {
    let mut source = damage(vec![slice(0, 0)]);
    if let GameEvent::DamageApplied { char_id, .. } = &mut source { *char_id = None; }
    rejects(&source);
    let mut loss = events().remove(4);
    let a = entity(1);
    let b = entity(3);
    if let GameEvent::LossSettled { final_blow, participants, .. } = &mut loss {
        *final_blow = Some(a);
        participants.push(KillParticipant { character_id: 1, entity: a, final_blow: true, damage: amount("1") });
        participants.push(KillParticipant { character_id: 3, entity: b, final_blow: true, damage: amount("1") });
    }
    rejects(&loss);
}
