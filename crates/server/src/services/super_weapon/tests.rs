use super::*;

#[test]
fn point_arg_accepts_exactly_three_finite_numeric_components() {
    for (value, expected) in [
        (Value::list([Value::Int(1), Value::Float(2.5), Value::Int(-3)]), Some([1.0, 2.5, -3.0])),
        (Value::tuple([Value::Float(0.0), Value::Int(0), Value::Float(-0.0)]), Some([0.0, 0.0, -0.0])),
        (Value::list([Value::Float(f64::NAN), Value::Int(1), Value::Int(2)]), None),
        (Value::list([Value::Int(1), Value::Float(f64::INFINITY), Value::Int(2)]), None),
        (Value::list([Value::Int(1), Value::Int(2)]), None),
        (Value::list([Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)]), None),
        (Value::list([Value::unicode("x"), Value::Int(2), Value::Int(3)]), None),
        (Value::Int(5), None),
        (Value::None, None),
    ] {
        assert_eq!(point_arg(Some(&value)), expected, "{value:?}");
    }
    assert_eq!(point_arg(None), None);
}

#[test]
fn refusals_carry_exact_client_notify_text() {
    use infinity_core::messages::super_weapon as sw;
    for (constant, text) in [
        (sw::UNSUPPORTED_TYPE, "该装备不支持此超级武器操作。"),
        (sw::INVALID_REQUEST, "无效的超级武器操作请求。"),
        (sw::WARP_LOCKED, "超级武器开火后的固定时间尚未结束，无法跃迁。"),
        (sw::JUMP_LOCKED, "超级武器开火后的禁止跳跃时间尚未结束，无法跳跃。"),
        (sw::CLOAK_LOCKED, "超级武器开火后的禁止隐形时间尚未结束，无法启动隐形装置。"),
    ] {
        assert_eq!(constant, text);
    }
    // Every positional refusal reaches the pilot as a CustomNotify payload.
    let CallError::User { dict, .. } = notify_error(sw::INVALID_REQUEST) else { panic!("refusal kind") };
    assert_eq!(dict, vec![("notify".into(), Value::unicode(sw::INVALID_REQUEST))]);
}

#[cfg(feature = "tests-effects")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn positional_refusals_cover_empire_mode_effect_and_type_without_fuel() {
    use crate::services::tests::fixture::{create_ship, ShipLocation, ShipItemSpec};
    use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_SHIP, FLAG_CARGO, FLAG_HI_SLOT_FIRST};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let charge = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let (point_type, slash_type) = (40631, 40632);
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[
        ShipItemSpec { type_id: point_type, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: slash_type, flag_id: FLAG_HI_SLOT_FIRST + 1, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: charge, flag_id: FLAG_CARGO, quantity: 7, singleton: false, online: false, attributes: &[] },
    ]).await.unwrap();
    let charge_item = f.items[2];
    async fn fuel(db: &crate::db::Db, charge_item: i64) -> i64 {
        db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&charge_item]).await.unwrap().get("quantity")
    }
    let (mut c, _peer) = crate::services::tests::client_with_sde(db.clone(), f.character, sde.clone()).await;
    let (empire, null) = {
        let mut systems = sde.map.systems();
        (systems.find(|s| s.security.is_some_and(|v| v > 0.0)).unwrap().solar_system_id,
            sde.map.systems().find(|s| s.security.is_some_and(|v| v <= 0.0)).unwrap().solar_system_id)
    };
    let point_args = |module, effect| vec![
        Value::Int(module), Value::Int(effect),
        Value::list([Value::Float(0.0), Value::Float(0.0), Value::Float(0.0)])];
    let slash_args = |module| vec![
        Value::Int(module),
        Value::list([Value::Float(0.0), Value::Float(0.0), Value::Float(0.0)]),
        Value::list([Value::Float(1.0), Value::Float(0.0), Value::Float(0.0)])];
    async fn call(c: &mut Client, method: &'static str, args: &[Value]) -> Option<CallResult> {
        let kwargs = Value::Dict(vec![]);
        dispatch(c, &Call { service: "superWeaponMgr", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
    }
    let notify_text = |result: Option<CallResult>| match result {
        Some(Err(CallError::User { dict, .. })) => dict.iter().find(|(k, _)| k == "notify")
            .and_then(|(_, v)| v.as_text()).map(|t| t.into_owned()),
        _ => None,
    };
    let unsupported = messages::super_weapon::UNSUPPORTED_TYPE;
    let invalid = messages::super_weapon::INVALID_REQUEST;
    let (point_module, slash_module) = (f.items[0], f.items[1]);
    // Empire space: the admitted cylinder's `disallowInEmpireSpace` refuses.
    c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)),
        ("solarsystemid", Value::Int(empire)), ("solarsystemid2", Value::Int(empire))]);
    let def = sde.super_weapons.get(point_type).unwrap();
    assert_eq!(notify_text(call(&mut c, "ActivateSinglePointTargetedModule", &point_args(point_module, i64::from(def.effect))).await),
        Some(unsupported.into()), "empire admission must refuse");
    // Mode mismatch: a slash call on the point-mode cylinder, and a point call
    // on the slash-mode slice; wrong effect id; a non-catalogue module type.
    assert_eq!(notify_text(call(&mut c, "ActivateSlashModule", &slash_args(point_module)).await), Some(unsupported.into()));
    assert_eq!(notify_text(call(&mut c, "ActivateSinglePointTargetedModule", &point_args(slash_module, i64::from(def.effect))).await),
        Some(unsupported.into()));
    assert_eq!(notify_text(call(&mut c, "ActivateSinglePointTargetedModule", &point_args(point_module, i64::from(def.effect) + 1)).await),
        Some(invalid.into()));
    assert_eq!(notify_text(call(&mut c, "ActivateSinglePointTargetedModule", &point_args(charge_item, i64::from(def.effect))).await),
        Some(unsupported.into()));
    // Null/wormhole space: the same admitted call clears the 1074 check; the
    // downstream lifecycle may still refuse, but never with UNSUPPORTED_TYPE.
    c.session.apply(vec![("solarsystemid", Value::Int(null)), ("solarsystemid2", Value::Int(null))]);
    let result = call(&mut c, "ActivateSinglePointTargetedModule", &point_args(point_module, i64::from(def.effect))).await;
    assert_ne!(notify_text(result), Some(unsupported.into()), "null-space admission must not refuse on 1074");
    // Every refusal happens before fuel admission: the charge is untouched.
    assert_eq!(fuel(&db, charge_item).await, 7);
    crate::services::tests::fixture::cleanup_ship(&db, None, f).await.unwrap();
}
