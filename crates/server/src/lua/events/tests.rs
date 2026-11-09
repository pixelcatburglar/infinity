use super::*;

#[test]
fn payload_contains_typed_named_scalars() -> mlua::Result<()> {
    let lua = Lua::new();
    let event = GameEvent::MarketOrderPlaced { char_id: 4, order_id: 5, type_id: 6, quantity: (1_i64 << 53) + 1, price: crate::isk::IskAmount::from_cents(150).unwrap(), is_buy: true };
    let table = event.to_lua(&lua)?;
    assert_eq!(table.get::<String>("kind")?, "MarketOrderPlaced");
    assert_eq!(table.get::<i64>("char_id")?, 4);
    assert_eq!(table.get::<mlua::Value>("quantity")?, mlua::Value::Integer((1_i64 << 53) + 1));
    assert_eq!(table.get::<f64>("price")?, 1.5);
    assert!(table.get::<bool>("is_buy")?);
    Ok(())
}

#[test]
fn pilot_event_payloads_and_cycle_outcomes_are_scalar() -> mlua::Result<()> {
    let lua = Lua::new();
    let events = [
        (GameEvent::Targeted { char_id: 1, by_ball_id: 2, system_id: 3 }, "Targeted", vec![("char_id", 1), ("by_ball_id", 2), ("system_id", 3)]),
        (GameEvent::Damaged { char_id: 1, ship_id: 2, attacker_ball_id: 3, system_id: 4 }, "Damaged", vec![("char_id", 1), ("ship_id", 2), ("attacker_ball_id", 3), ("system_id", 4)]),
        (GameEvent::WarpArrived { char_id: 1, system_id: 2 }, "WarpArrived", vec![("char_id", 1), ("system_id", 2)]),
    ];
    for (event, kind, fields) in events {
        assert!(known_kind(kind));
        assert_eq!(event.kind(), kind);
        let table = event.to_lua(&lua)?;
        assert_eq!(table.get::<String>("kind")?, kind);
        for (name, value) in &fields { assert_eq!(table.get::<i64>(*name)?, *value); }
        assert_eq!(table.pairs::<String, mlua::Value>().count(), fields.len() + 1);
    }
    assert!(known_kind("ModuleCycleEnded"));
    for (outcome, expected) in [
        (ModuleCycleOutcome::Completed, "completed"),
        (ModuleCycleOutcome::TargetLost, "target_lost"),
        (ModuleCycleOutcome::OutOfCapacitor, "out_of_capacitor"),
        (ModuleCycleOutcome::CargoFull, "cargo_full"),
    ] {
        let event = GameEvent::ModuleCycleEnded { char_id: 1, module_item_id: 2, effect_id: 3, outcome };
        assert_eq!(event.kind(), "ModuleCycleEnded");
        let table = event.to_lua(&lua)?;
        assert_eq!(table.get::<String>("kind")?, "ModuleCycleEnded");
        assert_eq!(table.get::<i64>("char_id")?, 1);
        assert_eq!(table.get::<i64>("module_item_id")?, 2);
        assert_eq!(table.get::<i64>("effect_id")?, 3);
        assert_eq!(table.get::<String>("outcome")?, expected);
        assert_eq!(table.pairs::<String, mlua::Value>().count(), 5);
    }
    Ok(())
}

#[test]
fn mining_resource_tables_preserve_slices_and_context() -> mlua::Result<()> {
    use super::resources::{FactContext, FactOrigin, MiningSlice};
    let lua = Lua::new();
    let context = FactContext { corporation_id: Some(7), alliance_id: None, faction_id: None, ship_id: Some(8), ship_type_id: None, system_id: Some(9), station_id: None, origin: FactOrigin::Qa };
    let event = GameEvent::OreMined { char_id: 1, ore_type: 2, context, slices: vec![
        MiningSlice { log_id: 3, occurred_at: 20, quantity: 4, volume_m3: 5.0 },
        MiningSlice { log_id: 6, occurred_at: 10, quantity: 7, volume_m3: 8.0 },
    ] };
    assert_eq!(event.resource_metadata().unwrap().0, 10);
    let table = event.to_lua(&lua)?;
    assert_eq!(table.get::<String>("kind")?, "OreMined");
    let context: Table = table.get("context")?;
    assert_eq!(context.get::<i64>("corporation_id")?, 7);
    assert_eq!(context.get::<String>("origin")?, "qa");
    assert!(context.get::<Option<i64>>("alliance_id")?.is_none());
    let slices: Table = table.get("slices")?;
    let row: Table = slices.get(2)?;
    assert_eq!(row.get::<i64>("occurred_at")?, 10);
    assert_eq!(row.get::<f64>("volume_m3")?, 8.0);
    Ok(())
}

#[test]
fn chip_exchange_reason_has_truthful_lua_label() -> mlua::Result<()> {
    use super::resources::{FactContext, FactOrigin, LoyaltyReason};
    let event = GameEvent::LoyaltyPointsEarned {
        char_id: 1, issuer_id: 2, amount: 3, reason: LoyaltyReason::ChipExchange, occurred_at: 4,
        context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Natural },
    };
    assert_eq!(event.to_lua(&Lua::new())?.get::<String>("reason")?, "chip_exchange");
    Ok(())
}
