use super::*;
use std::sync::Arc;

#[test]
fn acceptance_obeys_category_block_and_type_override() {
    let mining = descriptor(FLAG_GENERAL_MINING_HOLD).unwrap();
    assert!(accepts_class(mining, Some(CATEGORY_ASTEROID), Some(GROUP_ICE), 1));
    assert!(!accepts_class(mining, Some(CATEGORY_ASTEROID), Some(GROUP_TERRAN_ARTIFACTS), 1));
    assert!(accepts_class(mining, None, Some(GROUP_HARVESTABLE_CLOUD), 1));
    let ammo = descriptor(FLAG_SPECIALIZED_AMMO_HOLD).unwrap();
    assert!(accepts_class(ammo, None, None, TYPE_NANITE_REPAIR_PASTE));
    assert!(!accepts_class(ammo, None, None, 1));
    let maintenance = descriptor(FLAG_SHIP_HANGAR).unwrap();
    assert!(!accepts_class(maintenance, Some(CATEGORY_SHIP), Some(GROUP_CAPSULE), 1));
    assert!(accepts_class(maintenance, Some(CATEGORY_SHIP), Some(GROUP_FRIGATE), 1));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn capacity_refusal_and_mining_routing() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship = |attr| sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(attr).is_some_and(|v| v > 0.0)).expect("ship with hold").type_id;
    let escape = Fit::for_space_type(sde.clone(), ship(ATTR_FRIGATE_ESCAPE_BAY_CAPACITY), vec![]);
    assert_eq!(effective_capacity(&escape, FLAG_FRIGATE_ESCAPE_BAY), 1.0);
    assert_eq!(effective_capacity(&escape, -1), 0.0);
    let empty = Fit::for_space_type(sde.clone(), -1, vec![]);
    assert_eq!(effective_capacity(&empty, FLAG_FRIGATE_ESCAPE_BAY), 0.0);
    let frigate = sde.types.iter().find(|t| t.group_id == Some(GROUP_FRIGATE)).expect("frigate");
    assert!(matches!(check(&sde, FLAG_FRIGATE_ESCAPE_BAY, frigate.type_id, false, 2, 1.0, 0.0),
        Err(CallError::User { msg, dict }) if msg == messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE
            && dict == vec![("type".into(), Value::Int(frigate.type_id as i64)), ("free".into(), Value::Float(1.0)), ("required".into(), Value::Float(2.0))]));
    for (flag, group, attr) in [
        (FLAG_SPECIALIZED_ICE_HOLD, GROUP_ICE, ATTR_SPECIAL_ICE_HOLD_CAPACITY),
        (FLAG_SPECIALIZED_GAS_HOLD, GROUP_HARVESTABLE_CLOUD, ATTR_SPECIAL_GAS_HOLD_CAPACITY),
    ] {
        let fit = Fit::for_space_type(sde.clone(), ship(attr), vec![]);
        let item = sde.types.iter().find(|t| t.group_id == Some(group) && t.volume.is_some_and(|v| v > 0.0)).expect("mining output");
        let cap = effective_capacity(&fit, flag);
        assert!(cap > 0.0);
        assert_eq!(mining_flag(&sde, &fit, item.type_id), flag);
        let volume = item_volume(&sde, item.type_id, false, 1, None).unwrap();
        assert!(matches!(check(&sde, flag, item.type_id, false, 1, cap, cap),
            Err(CallError::User { msg, dict }) if msg == messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE
                && dict == vec![("type".into(), Value::Int(item.type_id as i64)), ("free".into(), Value::Float(0.0)), ("required".into(), Value::Float(volume))]));
        assert_eq!(mining_flag(&sde, &empty, item.type_id), FLAG_CARGO);
    }
    for (flag, category, name) in [
        (FLAG_CARGO, None, messages::MSG_NOT_ENOUGH_CARGO_SPACE),
        (FLAG_DRONE_BAY, Some(CATEGORY_DRONE), messages::MSG_NOT_ENOUGH_DRONE_BAY_SPACE),
        (FLAG_FIGHTER_BAY, Some(CATEGORY_FIGHTER), messages::MSG_NOT_ENOUGH_FIGHTER_BAY_SPACE),
    ] {
        let item = sde.types.iter().find(|t| category.is_none_or(|category| t.category_id == Some(category))
            && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)
            && item_volume(&sde, t.type_id, false, 1, None).is_ok_and(|v| v.is_finite() && v > 0.0)).unwrap();
        let volume = item_volume(&sde, item.type_id, false, 1, None).unwrap();
        let expected = if flag == FLAG_CARGO {
            vec![("available".into(), Value::Float(0.0)), ("volume".into(), Value::Float(volume))]
        } else {
            vec![("type".into(), Value::Int(item.type_id as i64)), ("free".into(), Value::Float(0.0)), ("required".into(), Value::Float(volume))]
        };
        assert!(matches!(check(&sde, flag, item.type_id, false, 1, 1.0, 1.0),
            Err(CallError::User { msg, dict }) if msg == name && dict == expected));
        for (quantity, capacity, used) in [(0, 1.0, 0.0), (-1, 1.0, 0.0), (1, f64::NAN, 0.0),
            (1, f64::INFINITY, 0.0), (1, -1.0, 0.0), (1, 1.0, f64::NAN),
            (1, 1.0, f64::INFINITY), (1, 1.0, -1.0)] {
            assert!(matches!(check(&sde, flag, item.type_id, false, quantity, capacity, used),
                Err(CallError::User { msg, dict }) if msg == messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED && dict.is_empty()));
        }
    }
    let blueprint = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_BLUEPRINT)
        && item_volume(&sde, t.type_id, true, -2, None).is_ok_and(|v| v.is_finite() && v > 0.0))
        .expect("blueprint with positive volume");
    let volume = item_volume(&sde, blueprint.type_id, true, -2, None).unwrap();
    assert_eq!(volume, item_volume(&sde, blueprint.type_id, true, 1, None).unwrap());
    for flag in [FLAG_CARGO, FLAG_EXPEDITION_HOLD] {
        assert!(check(&sde, flag, blueprint.type_id, true, -2, volume, 0.0).is_ok());
        for used in [volume / 2.0, volume] {
            let free = volume - used;
            let (name, expected) = if flag == FLAG_CARGO {
                (messages::MSG_NOT_ENOUGH_CARGO_SPACE,
                    vec![("available".into(), Value::Float(free)), ("volume".into(), Value::Float(volume))])
            } else {
                (messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE,
                    vec![("type".into(), Value::Int(blueprint.type_id as i64)), ("free".into(), Value::Float(free)), ("required".into(), Value::Float(volume))])
            };
            assert!(matches!(check(&sde, flag, blueprint.type_id, true, -2, volume, used),
                Err(CallError::User { msg, dict }) if msg == name && dict == expected));
        }
        assert!(matches!(check(&sde, flag, blueprint.type_id, false, -2, volume, 0.0),
            Err(CallError::User { msg, dict }) if msg == messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED && dict.is_empty()));
    }
    let wrong = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_DRONE)).unwrap();
    assert!(matches!(check(&sde, FLAG_DRONE_BAY, wrong.type_id, false, 1, 1.0, 0.0),
        Err(CallError::User { msg, dict }) if msg == messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED && dict.is_empty()));
    if let Some(zero) = sde.types.iter().find(|t| item_volume(&sde, t.type_id, true, 1, None).is_ok_and(|v| v == 0.0)) {
        assert!(check(&sde, FLAG_CARGO, zero.type_id, true, 1, 1.0, 0.0).is_ok());
        assert!(check(&sde, FLAG_CARGO, zero.type_id, true, 1, 0.0, 0.0).is_err());
    };
}
