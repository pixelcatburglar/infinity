use super::*;

#[test]
fn payload_contains_typed_named_scalars() -> mlua::Result<()> {
    let lua = Lua::new();
    let event = GameEvent::MarketOrderPlaced { char_id: 4, order_id: 5, type_id: 6, quantity: 7, price: 1.5, is_buy: true };
    let table = event.to_lua(&lua)?;
    assert_eq!(table.get::<String>("kind")?, "MarketOrderPlaced");
    assert_eq!(table.get::<i64>("char_id")?, 4);
    assert_eq!(table.get::<i32>("quantity")?, 7);
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
