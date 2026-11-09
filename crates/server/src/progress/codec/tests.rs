use super::*;
use crate::lua::events::ModuleCycleOutcome;

#[test]
fn all_event_variants_round_trip() {
    let events = [
        GameEvent::CharacterSelected { user_id: 1, char_id: 2 },
        GameEvent::CharacterLoggedOff { user_id: 1, char_id: 2 },
        GameEvent::Undocked { char_id: 1, ship_id: 2, station_id: 3, system_id: 4 },
        GameEvent::Docked { char_id: 1, ship_id: 2, station_id: 3, system_id: 4 },
        GameEvent::Jumped { char_id: 1, ship_id: 2, from_system_id: 3, to_system_id: 4 },
        GameEvent::ShipLost { char_id: 1, ship_id: 2, type_id: 3, system_id: 4 },
        GameEvent::NpcKilled { char_id: 1, npc_id: 2, type_id: 3, system_id: 4 },
        GameEvent::ItemsMoved { char_id: 1, item_id: 2, from_location_id: 3, to_location_id: 4, quantity: (1_i64 << 53) + 1 },
        GameEvent::WalletChanged { owner_id: 1, account_key: 2, balance: crate::isk::IskAmount::from_cents(350).unwrap(), delta: crate::isk::IskAmount::from_cents(-450).unwrap() },
        GameEvent::SkillTrained { char_id: 1, type_id: 2, level: 3 },
        GameEvent::SkillStateChanged { char_id: 1 },
        GameEvent::MarketOrderPlaced { char_id: 1, order_id: 2, type_id: 3, quantity: (1_i64 << 53) + 1, price: crate::isk::IskAmount::from_cents(550).unwrap(), is_buy: true },
        GameEvent::MarketOrderFilled { order_id: 1, buyer_id: 2, seller_id: 3, type_id: 4, quantity: (1_i64 << 53) + 1, price: crate::isk::IskAmount::from_cents(650).unwrap() },
        GameEvent::MissionAccepted { char_id: 1, agent_id: 2, mission_id: 3 },
        GameEvent::MissionCompleted { char_id: 1, agent_id: 2, mission_id: 3 },
        GameEvent::BallparkBound { char_id: 1, system_id: 2 },
        GameEvent::Targeted { char_id: 1, by_ball_id: 2, system_id: 3 },
        GameEvent::Damaged { char_id: 1, ship_id: 2, attacker_ball_id: 3, system_id: 4 },
        GameEvent::ModuleCycleEnded { char_id: 1, module_item_id: 2, effect_id: 3, outcome: ModuleCycleOutcome::TargetLost },
        GameEvent::WarpArrived { char_id: 1, system_id: 2 },
    ];
    for event in events {
        let payload = encode(&event).unwrap();
        assert_eq!(decode(SCHEMA_VERSION, event.kind(), &payload).unwrap(), event);
        assert!(decode(SCHEMA_VERSION + 1, event.kind(), &payload).is_err());
        assert!(decode(SCHEMA_VERSION, "Unknown", &payload).is_err());
    }
}

#[test]
fn legacy_item_movement_payload_decodes() {
    let payload = r#"{"ItemsMoved":{"char_id":1,"item_id":2,"from_location_id":3,"to_location_id":4,"quantity":5}}"#;
    assert_eq!(decode(SCHEMA_VERSION, "ItemsMoved", payload).unwrap(), GameEvent::ItemsMoved {
        char_id: 1, item_id: 2, from_location_id: 3, to_location_id: 4, quantity: 5,
    });
}

#[test]
fn malformed_payload_is_rejected() {
    assert!(decode(SCHEMA_VERSION, "MarketOrderPlaced", r#"{"MarketOrderPlaced":{"char_id":1,"order_id":2,"type_id":3,"quantity":1,"price":null,"is_buy":true}}"#).is_err());
    assert!(decode(SCHEMA_VERSION, "WalletChanged", "{\"WalletChanged\":{\"owner_id\":1,\"account_key\":1,\"balance\":null,\"delta\":0}}").is_err());
}

#[test]
fn exact_wallet_payload_and_lua_projection() {
    let amount = crate::isk::IskAmount::parse_decimal("9999999999999999999999999999999999.99").unwrap();
    let event = GameEvent::WalletChanged { owner_id: 1, account_key: 2, balance: amount, delta: amount };
    let payload = encode(&event).unwrap();
    assert!(payload.contains("\"balance\":\"9999999999999999999999999999999999.99\""));
    assert_eq!(decode(SCHEMA_VERSION, "WalletChanged", &payload).unwrap(), event);
    assert!(decode(1, "WalletChanged", &payload).is_err());
    let lua = mlua::Lua::new();
    let table = event.to_lua(&lua).unwrap();
    let balance: f64 = table.get("balance").unwrap();
    assert_eq!(balance, amount.wire_isk());
    assert!(balance > crate::isk::personal_client_cap().wire_isk());
}

#[test]
fn exact_market_price_round_trip_and_lua_projection() {
    let amount = crate::isk::IskAmount::parse_decimal("9999999999999999999999999999999999.99").unwrap();
    let event = GameEvent::MarketOrderFilled { order_id: 1, buyer_id: 2, seller_id: 3, type_id: 4, quantity: 5, price: amount };
    let payload = encode(&event).unwrap();
    assert!(payload.contains("\"price\":\"9999999999999999999999999999999999.99\""));
    assert_eq!(decode(SCHEMA_VERSION, event.kind(), &payload).unwrap(), event);
    let lua = mlua::Lua::new();
    let table = event.to_lua(&lua).unwrap();
    assert_eq!(table.get::<f64>("price").unwrap(), amount.wire_isk());
}

#[test]
fn resource_facts_round_trip_and_reject_bad_slices() {
    use crate::lua::events::resources::{FactContext, FactOrigin, MiningSlice, HackKind, LoyaltyReason};
    let context = FactContext { corporation_id: Some(2), alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(3), station_id: None, origin: FactOrigin::Natural };
    let slice = MiningSlice { log_id: 1, occurred_at: 2, quantity: 3, volume_m3: 4.0 };
    let events = vec![
        GameEvent::OreMined { char_id: 1, ore_type: 2, context: context.clone(), slices: vec![slice.clone()] },
        GameEvent::GasHarvested { char_id: 1, gas_type: 2, context: context.clone(), slices: vec![slice.clone()] },
        GameEvent::ManufacturingInstalled { char_id: 1, job_id: 2, owner_id: 1, owner_is_corporation: false, product_type: 3, runs: 2, scheduled_quantity: 4, facility_id: 5, occurred_at: 6, context: context.clone() },
        GameEvent::ManufacturingDelivered { char_id: 1, installer_id: 2, job_id: 3, owner_id: 4, owner_is_corporation: true, product_type: 5, quantity: 6, facility_id: 7, output_location_id: 8, occurred_at: 9, context: context.clone() },
        GameEvent::SignatureResolved { char_id: 1, site_id: 2, signature_id: "ABC".into(), signature_type: 3, archetype: None, dungeon_type: Some(4), occurred_at: 5, context: context.clone() },
        GameEvent::WreckSalvaged { char_id: 1, wreck_id: 2, wreck_type: 3, occurred_at: 4, context: context.clone() },
        GameEvent::ContainerHacked { char_id: 1, container_id: 2, container_type: 3, site_id: 4, archetype: None, hack_kind: HackKind::Data, occurred_at: 5, context: context.clone() },
        GameEvent::LoyaltyPointsEarned { char_id: 1, issuer_id: 2, amount: 3, reason: LoyaltyReason::MissionReward, occurred_at: 4, context: context.clone() },
    ];
    for event in events {
        let payload = encode(&event).unwrap();
        assert_eq!(decode(SCHEMA_VERSION, event.kind(), &payload).unwrap(), event);
        assert!(decode(SCHEMA_VERSION + 1, event.kind(), &payload).is_err());
    }
    for slices in [vec![], vec![MiningSlice { quantity: 0, ..slice.clone() }], vec![MiningSlice { volume_m3: f64::INFINITY, ..slice.clone() }], vec![MiningSlice { occurred_at: 0, ..slice.clone() }], vec![MiningSlice { quantity: i64::MAX, ..slice.clone() }, slice.clone()]] {
        assert!(encode(&GameEvent::OreMined { char_id: 1, ore_type: 2, context: context.clone(), slices }).is_err());
    }
    assert!(decode(SCHEMA_VERSION, "ContainerHacked", r#"{"ContainerHacked":{"char_id":1,"container_id":2,"container_type":3,"site_id":4,"archetype":null,"hack_kind":"Unknown","occurred_at":5,"context":{"corporation_id":null,"alliance_id":null,"faction_id":null,"ship_id":null,"ship_type_id":null,"system_id":null,"station_id":null,"origin":"Natural"}}}"#).is_err());
}

#[test]
fn old_payload_representation_is_unchanged() {
    let event = GameEvent::MissionCompleted { char_id: 1, agent_id: 2, mission_id: 3 };
    let payload = r#"{"MissionCompleted":{"char_id":1,"agent_id":2,"mission_id":3}}"#;
    assert_eq!(encode(&event).unwrap(), payload);
    assert_eq!(decode(SCHEMA_VERSION, "MissionCompleted", payload).unwrap(), event);
}

#[test]
fn manufacturing_rejects_invalid_counts_and_times() {
    use crate::lua::events::resources::{FactContext, FactOrigin};
    let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Natural };
    let event = GameEvent::ManufacturingInstalled { char_id: 1, job_id: 2, owner_id: 1, owner_is_corporation: false, product_type: 3, runs: 0, scheduled_quantity: 1, facility_id: 4, occurred_at: 5, context: context.clone() };
    assert!(encode(&event).is_err());
    let event = GameEvent::ManufacturingDelivered { char_id: 1, installer_id: 1, job_id: 2, owner_id: 1, owner_is_corporation: false, product_type: 3, quantity: 0, facility_id: 4, output_location_id: 4, occurred_at: 5, context };
    assert!(encode(&event).is_err());
}

#[test]
fn mining_ids_and_loyalty_sources_are_validated_on_encode_and_decode() {
    use crate::lua::events::resources::{FactContext, FactOrigin, LoyaltyReason, MiningSlice};
    let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Natural };
    let slice = MiningSlice { log_id: 1, occurred_at: 2, quantity: 1, volume_m3: 1.0 };
    let mining = |slices| GameEvent::OreMined { char_id: 1, ore_type: 2, context: context.clone(), slices };
    let valid = mining(vec![slice.clone(), MiningSlice { log_id: 2, ..slice.clone() }]);
    let payload = encode(&valid).unwrap();
    assert_eq!(decode(SCHEMA_VERSION, valid.kind(), &payload).unwrap(), valid);
    for event in [mining(vec![MiningSlice { log_id: 0, ..slice.clone() }]), mining(vec![slice.clone(), slice])] {
        assert!(encode(&event).is_err());
        let payload = serde_json::to_string(&event).unwrap();
        assert!(decode(SCHEMA_VERSION, event.kind(), &payload).is_err());
    }
    for (issuer_id, amount, reason, occurred_at, origin) in [
        (2, 0, LoyaltyReason::MissionReward, 1, FactOrigin::Natural),
        (2, -1, LoyaltyReason::MissionReward, 1, FactOrigin::Natural),
        (2, 1, LoyaltyReason::GmAdjustment, 1, FactOrigin::Natural),
        (2, 1, LoyaltyReason::StorePurchase, 1, FactOrigin::Natural),
        (2, 1, LoyaltyReason::ChipExchange, 1, FactOrigin::Natural),
        (2, 1, LoyaltyReason::DailyReward, 1, FactOrigin::Natural),
        (infinity_core::consts::CORP_HERALDRY, 1, LoyaltyReason::DailyReward, 1, FactOrigin::Natural),
        (2, 1, LoyaltyReason::MissionReward, 1, FactOrigin::Gm),
        (2, 1, LoyaltyReason::MissionReward, 1, FactOrigin::Qa),
        (2, 1, LoyaltyReason::MissionReward, 0, FactOrigin::Natural),
        (infinity_core::consts::CORP_HERALDRY, 1, LoyaltyReason::MissionReward, 1, FactOrigin::Natural),
    ] {
        let event = GameEvent::LoyaltyPointsEarned { char_id: 1, issuer_id, amount, reason, occurred_at, context: FactContext { origin, ..context.clone() } };
        assert!(encode(&event).is_err());
        let payload = serde_json::to_string(&event).unwrap();
        assert!(decode(SCHEMA_VERSION, event.kind(), &payload).is_err());
    }
}
