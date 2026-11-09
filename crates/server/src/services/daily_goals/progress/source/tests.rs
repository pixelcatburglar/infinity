use super::*;
use crate::lua::events::{combat::{CombatSlice, CombatEntity}, resources::MiningSlice};

fn context(origin: FactOrigin) -> FactContext {
    FactContext { corporation_id: Some(8), alliance_id: None, faction_id: None, ship_id: Some(9), ship_type_id: Some(10), system_id: Some(11), station_id: None, origin }
}
fn entity(character: Option<i64>) -> CombatEntity {
    CombatEntity { item_id: 2, type_id: 3, incarnation: [1; 16], character_id: character, corporation_id: Some(4), alliance_id: Some(5), faction_id: Some(6) }
}
fn combat_slice(at: i64) -> CombatSlice {
    CombatSlice { index: 0, qualification_epoch: 0, boundary_at: at, active_until: at, first_occurred_at: at, last_occurred_at: at, successful_count: 1, amount: BinaryAmount { coefficient: "3".into(), exponent: -1 } }
}
fn envelope(event: GameEvent, subject: Option<i64>, key: &str, business: bool) -> EventEnvelope {
    let meta = event.resource_metadata().or_else(|| crate::lua::events::combat::metadata(&event));
    let (occurred_at, context, origin) = match meta {
        Some((at, ctx)) => (at, serde_json::to_string(ctx).unwrap(), ctx.origin.as_str().to_owned()),
        None => (10, "{}".into(), "server".into()),
    };
    EventEnvelope { event_id: "event".into(), schema_version: SCHEMA_VERSION, kind: event.kind().into(), payload: String::new(), event, occurred_at, recorded_at: 20,
        source_namespace: if business { "business" } else { "invocation" }.into(), source_key: key.into(), ordinal: 0, context, causation: None, origin, subject_character_id: subject }
}
fn project(event: GameEvent, key: &str, business: bool) -> Fact {
    map(&envelope(event, Some(1), key, business), &Sde::default()).unwrap().unwrap()
}

#[test]
fn capability_tracks_subscribed_native_mappings() {
    for key in ["KillNPC", "DamageShip", "RemoteRepairShields", "RemoteRepairArmor", "MineOre", "ScanSignature", "ManufactureItem", "SalvageWreck", "EarnLoyaltyPoints", "SpaceJump"] {
        assert!(supports(key), "{key}");
    }
    for key in ["CaptureFWComplex", "DefendFWComplex", "CompleteGoal"] {
        assert!(!supports(key), "{key}");
    }
    assert_eq!(method("RemoteRepairApplied", Some(HpLayer::Hull)), None);
}
#[test]
fn projects_native_methods_and_exact_slices() {
    let ctx = context(FactOrigin::Natural);
    let damage = GameEvent::DamageApplied { char_id: Some(1), context: ctx, source: entity(Some(1)), target: entity(Some(7)), module_id: None, weapon_type_id: None, effect_id: None, layer: HpLayer::Hull, slices: vec![combat_slice(10), combat_slice(20)] };
    let f = project(damage, "DamageApplied:1:11:0:0:0", true);
    assert_eq!(f.input.method, "DamageShip"); assert_eq!(f.input.character, Some(7)); assert_eq!(f.input.corporation, Some(4));
    assert_eq!(f.slices, vec![Slice { occurred_at: 10, amount: Increment::Hp(BinaryAmount { coefficient: "3".into(), exponent: -1 }) }, Slice { occurred_at: 20, amount: Increment::Hp(BinaryAmount { coefficient: "3".into(), exponent: -1 }) }]);
    for (layer, name) in [(HpLayer::Shield, "RemoteRepairShields"), (HpLayer::Armor, "RemoteRepairArmor")] {
        let event = GameEvent::RemoteRepairApplied { char_id: Some(1), context: ctx, source: entity(Some(1)), target: entity(Some(7)), module_id: None, effect_id: None, layer, slices: vec![combat_slice(10)] };
        assert_eq!(project(event, "RemoteRepairApplied:1:11:0:0:0", true).input.method, name);
    }
    let events = [
        (GameEvent::NpcDestroyed { death_id: "NpcDestroyed:death".into(), spawn_generation: 1, npc: entity(None), system_id: 11, killer_id: Some(1), killer: Some(entity(Some(1))), context: ctx, occurred_at: 10, base_bounty: BinaryAmount { coefficient: "0".into(), exponent: 0 }, static_version: None }, "NpcDestroyed:death", true, "KillNPC", 1),
        (GameEvent::OreMined { char_id: 1, ore_type: 12, context: ctx, slices: vec![MiningSlice { log_id: 13, occurred_at: 10, quantity: 14, volume_m3: 99.0 }, MiningSlice { log_id: 15, occurred_at: 20, quantity: 16, volume_m3: 98.0 }] }, "OreMined:13", true, "MineOre", 14),
        (GameEvent::SignatureResolved { char_id: 1, site_id: 2, signature_id: "ABC".into(), signature_type: 3, archetype: None, dungeon_type: None, occurred_at: 10, context: ctx }, "0", false, "ScanSignature", 1),
        (GameEvent::WreckSalvaged { char_id: 1, wreck_id: 2, wreck_type: 3, occurred_at: 10, context: ctx }, "WreckSalvaged:2", true, "SalvageWreck", 1),
        (GameEvent::LoyaltyPointsEarned { char_id: 1, issuer_id: 2, amount: 3, reason: LoyaltyReason::MissionReward, occurred_at: 10, context: ctx }, "0", false, "EarnLoyaltyPoints", 3),
        (GameEvent::Jumped { char_id: 1, ship_id: 2, from_system_id: 3, to_system_id: 4 }, "Jumped:5", true, "SpaceJump", 1),
    ];
    for (event, key, business, method, amount) in events {
        let f = project(event, key, business); assert_eq!(f.input.method, method); assert_eq!(f.slices[0], Slice { occurred_at: 10, amount: Increment::Units(amount) });
        if method == "MineOre" { assert_eq!(f.slices[1], Slice { occurred_at: 20, amount: Increment::Units(16) }); }
        if method == "SpaceJump" { assert_eq!(f.input.system, Some(4)); }
    }
    let manufacturing = GameEvent::ManufacturingInstalled { char_id: 1, job_id: 2, owner_id: 1, owner_is_corporation: false, product_type: 3, runs: 2, scheduled_quantity: 9, facility_id: 4, occurred_at: 10, context: ctx };
    assert!(map(&envelope(manufacturing, Some(1), "ManufacturingInstalled:2", true), &Sde::default()).is_err());
}
#[test]
fn rejects_inconsistent_envelopes_and_recognizes_ineligible_sources() {
    let ctx = context(FactOrigin::Natural);
    let event = GameEvent::WreckSalvaged { char_id: 1, wreck_id: 2, wreck_type: 3, occurred_at: 10, context: ctx };
    let mut e = envelope(event, Some(1), "WreckSalvaged:2", true);
    e.context = "{\"origin\":\"Natural\"}".into(); assert!(map(&e, &Sde::default()).is_err());
    e.context = serde_json::to_string(&ctx).unwrap(); e.origin = "gm".into(); assert!(map(&e, &Sde::default()).is_err());
    e.origin = "natural".into(); e.subject_character_id = Some(2); assert!(map(&e, &Sde::default()).is_err());
    e.subject_character_id = Some(1); e.source_key = "WreckSalvaged:3".into(); assert!(map(&e, &Sde::default()).is_err());
    for origin in [FactOrigin::Gm, FactOrigin::Qa] {
        let event = GameEvent::WreckSalvaged { char_id: 1, wreck_id: 2, wreck_type: 3, occurred_at: 10, context: context(origin) };
        assert!(map(&envelope(event, Some(1), "WreckSalvaged:2", true), &Sde::default()).unwrap().is_none());
    }
    let event = GameEvent::RemoteRepairApplied { char_id: Some(1), context: ctx, source: entity(Some(1)), target: entity(Some(7)), module_id: None, effect_id: None, layer: HpLayer::Hull, slices: vec![combat_slice(10)] };
    assert!(map(&envelope(event, Some(1), "RemoteRepairApplied:1:11:0:0:0", true), &Sde::default()).unwrap().is_none());
    let event = GameEvent::NpcDestroyed { death_id: "NpcDestroyed:death".into(), spawn_generation: 1, npc: entity(None), system_id: 11, killer_id: None, killer: None, context: ctx, occurred_at: 10, base_bounty: BinaryAmount { coefficient: "0".into(), exponent: 0 }, static_version: None };
    assert!(map(&envelope(event, None, "NpcDestroyed:death", true), &Sde::default()).unwrap().is_none());
    let jump = GameEvent::Jumped { char_id: 1, ship_id: 2, from_system_id: 3, to_system_id: 4 };
    let mut e = envelope(jump, Some(1), "Jumped:5", true); e.origin = "natural".into(); assert!(map(&e, &Sde::default()).is_err());
}

#[test]
fn npc_combat_without_player_is_ineligible_but_identity_mismatches_fail() {
    let event = GameEvent::DamageApplied { char_id: None, context: context(FactOrigin::Natural), source: entity(None), target: entity(Some(7)), module_id: None, weapon_type_id: None, effect_id: None, layer: HpLayer::Shield, slices: vec![combat_slice(10)] };
    let mut e = envelope(event, None, "DamageApplied:1:11:0:0:0", true);
    assert!(map(&e, &Sde::default()).unwrap().is_none());
    e.subject_character_id = Some(1);
    assert!(map(&e, &Sde::default()).is_err());
    e.subject_character_id = None;
    if let GameEvent::DamageApplied { source, .. } = &mut e.event { source.character_id = Some(1); }
    assert!(map(&e, &Sde::default()).is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE; run serially"]
async fn manufacturing_projects_real_station_owner_and_scheduled_quantity() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let station = sde.map.stations().next().expect("initialized SDE must contain stations").station_id;
    for (owner_id, corporation, owner) in [(1, false, Owner::Character), (8, true, Owner::Corporation)] {
        let event = GameEvent::ManufacturingInstalled { char_id: 1, job_id: 2, owner_id, owner_is_corporation: corporation, product_type: 3, runs: 2, scheduled_quantity: 9, facility_id: station, occurred_at: 10, context: context(FactOrigin::Natural) };
        let fact = map(&envelope(event, Some(1), "ManufacturingInstalled:2", true), &sde).unwrap().unwrap();
        assert_eq!(fact.character, 1);
        assert_eq!(fact.input.method, "ManufactureItem");
        assert_eq!(fact.input.type_id, Some(3));
        assert_eq!(fact.input.owner, Some(owner));
        assert_eq!(fact.input.facility, Some(station as u64));
        assert_eq!(fact.input.facility_is_station, Some(true));
        assert_eq!(fact.slices, vec![Slice { occurred_at: 10, amount: Increment::Units(9) }]);
    }
}
