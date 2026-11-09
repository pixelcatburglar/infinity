//! Actual inventory moves of SDE armor and shield fitting modifiers.
use infinity_core::{
    consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_ARMOR_HP,
    ATTR_HEAT_LOW, ATTR_HEAT_MED, ATTR_HEAT_HI, ATTR_HULL_DAMAGE, ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE},
};
use crate::{
    services::{combat, fit::Key},
};
use marshal::Value;
use super::fixture::{self, MovementFixture};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn plate_install_remove_and_move_between_hulls() {
    let f = MovementFixture::create(ATTR_ARMOR_HP).await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let task = tokio::spawn(async move {
        let mut f = f;
        let ship = f.rows.ship;
        let other = f.second_ship;
        let owner = f.rows.owner;
        let module = f.rows.modules[0];
        let base = f.value(ship, ATTR_ARMOR_HP).await;
        let other_base = f.value(other, ATTR_ARMOR_HP).await;
        for _ in 0..2 {
            assert_eq!(f.move_to(ship, ship, f.slot).await.ok().flatten(), Some(module));
            let installed = f.value(ship, ATTR_ARMOR_HP).await;
            let events = f.drain();
            assert_eq!(fixture::attribute_rows(&events, owner, ship, ATTR_ARMOR_HP),
                if f.requires_online { vec![] } else { vec![(base, installed)] });
            assert!(fixture::attribute_rows(&events, owner, ship, ATTR_SHIELD_CAPACITY).is_empty());
            let item_event = events.iter().position(|(name, _)| name == "OnItemChange").expect("inventory change");
            let batch = events.iter().position(|(name, _)| name == "OnModuleAttributeChanges").expect("effective batch");
            assert!(item_event < batch, "inventory row must precede effective values");
            let [Value::List(rows)] = events[batch].1.as_slice() else { panic!("effective attribute batch") };
            assert!(rows.iter().any(|row| matches!(row, Value::Tuple(fields)
                if fields.get(1) == Some(&Value::Int(owner)) && fields.get(2) == Some(&Value::Int(module)))));
            if f.requires_online {
                assert_eq!(installed, base);
                f.online(ship).await;
            }
            let effective = f.value(ship, ATTR_ARMOR_HP).await;
            assert!(effective > base, "selected SDE armor module must increase armor HP");
            if f.requires_online {
                assert_eq!(fixture::attribute_rows(&f.drain(), owner, ship, ATTR_ARMOR_HP), vec![(base, effective)]);
            }
            assert_eq!(f.move_to(ship, ship, fixture::CARGO).await.ok().flatten(), Some(module));
            let removed = f.value(ship, ATTR_ARMOR_HP).await;
            assert_eq!(removed, base);
            assert_eq!(fixture::attribute_rows(&f.drain(), owner, ship, ATTR_ARMOR_HP), vec![(effective, removed)]);
        }
        assert_eq!(f.move_to(ship, ship, f.slot).await.ok().flatten(), Some(module));
        if f.requires_online { f.online(ship).await; }
        let installed = f.value(ship, ATTR_ARMOR_HP).await;
        f.drain();
        assert_eq!(f.move_to(ship, ship, f.slot + 1).await.ok().flatten(), Some(module));
        let row = f.client.db.query_one("SELECT location_id,flag_id FROM game.items WHERE item_id=$1", &[&module]).await.unwrap();
        assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (ship, f.slot + 1));
        assert_eq!(f.value(ship, ATTR_ARMOR_HP).await, installed);
        let events = f.drain();
        assert!(events.iter().any(|(name, _)| name == "OnItemChange"));
        assert!(fixture::attribute_rows(&events, owner, ship, ATTR_ARMOR_HP).is_empty());
        assert!(events.iter().all(|(name, _)| name != "OnModuleAttributeChanges"));
        assert_eq!(f.move_to(other, ship, f.slot).await.ok().flatten(), Some(module));
        let source = f.value(ship, ATTR_ARMOR_HP).await;
        let destination = f.value(other, ATTR_ARMOR_HP).await;
        assert_eq!(source, base);
        assert_eq!(destination > other_base, !f.requires_online);
        let events = f.drain();
        assert_eq!(fixture::attribute_rows(&events, owner, ship, ATTR_ARMOR_HP), vec![(installed, source)]);
        assert_eq!(fixture::attribute_rows(&events, owner, other, ATTR_ARMOR_HP),
            if f.requires_online { vec![] } else { vec![(other_base, destination)] });
        assert!(events.iter().all(|(name, _)| name != "OnGodmaPrimeLocation"));
        for hull in if f.requires_online { vec![ship] } else { vec![ship, other] } {
            assert_eq!(events.iter().filter(|(name, args)| name == "OnModuleAttributeChanges"
                && args.iter().any(|arg| matches!(arg, Value::List(rows) if rows.iter().any(|row|
                    matches!(row, Value::Tuple(fields) if fields.get(2) == Some(&Value::Int(hull))))))).count(), 1);
        }
        let before = super::super::fixture::fingerprint(&f.client.db).await.unwrap();
        assert!(f.move_to(ship, ship, f.slot).await.is_err());
        assert_eq!(super::super::fixture::fingerprint(&f.client.db).await.unwrap(), before);
        assert!(f.drain().is_empty());
        assert_eq!(f.move_to(ship, other, f.slot).await.ok().flatten(), Some(module));
        if f.requires_online { f.online(ship).await; }
        assert_eq!(f.value(ship, ATTR_ARMOR_HP).await, installed);
        assert_eq!(f.value(other, ATTR_ARMOR_HP).await, other_base);
    });
    let result = task.await;
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn shield_extender_unfit_checkpoints_and_reload_preserves_damage() {
    let f = MovementFixture::create(ATTR_SHIELD_CAPACITY).await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let task = tokio::spawn(async move {
        let mut f = f;
        let ship = f.rows.ship;
        let owner = f.rows.owner;
        let module = f.rows.modules[0];
        let small = f.value(ship, ATTR_SHIELD_CAPACITY).await;
        assert_eq!(f.move_to(ship, ship, f.slot).await.ok().flatten(), Some(module));
        if f.requires_online { f.online(ship).await; }
        let large = f.value(ship, ATTR_SHIELD_CAPACITY).await;
        assert!(large > small, "selected SDE extender must increase shield capacity");
        f.drain();
        let fit = f.fit(ship).await;
        let armor = fit.value(Key::Ship, ATTR_ARMOR_HP) * 0.1;
        let hull = fit.value(Key::Ship, infinity_core::consts::dogma::ATTR_STRUCTURE_HP) * 0.1;
        let saved = (small + large) / 2.0;
        for (attr, value) in [(ATTR_SHIELD_CHARGE, saved), (ATTR_ARMOR_DAMAGE, armor), (ATTR_HULL_DAMAGE, hull),
            (ATTR_HEAT_LOW, 0.1), (ATTR_HEAT_MED, 0.2), (ATTR_HEAT_HI, 0.3)] {
            assert!(crate::dao::items::dogma::set_override(&f.client.db, ship, attr, value).await.unwrap());
        }
        assert_eq!(f.move_to(ship, ship, fixture::CARGO).await.ok().flatten(), Some(module));
        let events = f.drain();
        assert_eq!(f.value(ship, ATTR_SHIELD_CAPACITY).await, small);
        assert_eq!(fixture::attribute_rows(&events, owner, ship, ATTR_SHIELD_CAPACITY), vec![(large, small)]);
        let shield = fixture::attribute_rows(&events, owner, ship, ATTR_SHIELD_CHARGE);
        assert_eq!(shield.len(), 1);
        assert_eq!(shield[0].1, small);
        assert!(shield[0].0 >= saved && shield[0].0 > small);
        let batch = events.iter().find_map(|(name, args)| {
            if name != "OnModuleAttributeChanges" { return None; }
            let [Value::List(rows)] = args.as_slice() else { return None; };
            rows.iter().position(|row| matches!(row, Value::Tuple(fields)
                if fields.get(2) == Some(&Value::Int(ship)) && fields.get(3) == Some(&Value::Int(i64::from(ATTR_SHIELD_CAPACITY)))))
                .map(|position| (rows, position))
        }).expect("shield maximum batch");
        assert!(matches!(batch.0.get(batch.1 + 1), Some(Value::Tuple(row))
            if row.get(2) == Some(&Value::Int(ship)) && row.get(3) == Some(&Value::Int(i64::from(ATTR_SHIELD_CHARGE)))));
        let overrides = crate::dao::items::dogma::overrides(&f.client.db, ship).await.unwrap();
        let value = |attr| overrides.iter().find(|row| row.attribute_id == attr).expect("saved attribute").value;
        assert_eq!(value(ATTR_SHIELD_CHARGE), shield[0].1);
        assert!((value(ATTR_ARMOR_DAMAGE) - armor).abs() < 1e-9);
        assert!((value(ATTR_HULL_DAMAGE) - hull).abs() < 1e-9);
        for (attr, expected) in [(ATTR_HEAT_LOW, 0.1), (ATTR_HEAT_MED, 0.2), (ATTR_HEAT_HI, 0.3)] { assert_eq!(value(attr), expected); }
        let fit = f.fit(ship).await;
        let restored = combat::prepare_condition_with_fit(&f.client.db, ship, &fit).await.ok().unwrap().scalars();
        assert_eq!(restored.0, small);
        assert!((restored.1 - armor).abs() < 1e-9 && (restored.2 - hull).abs() < 1e-9);
        assert_eq!(f.move_to(ship, ship, f.slot).await.ok().flatten(), Some(module));
        if f.requires_online { f.online(ship).await; }
        let events = f.drain();
        let pairs = fixture::attribute_rows(&events, owner, ship, ATTR_SHIELD_CHARGE);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].1 < large, "refit must not fill shield to the enlarged maximum");
        let fit = f.fit(ship).await;
        let restored = combat::prepare_condition_with_fit(&f.client.db, ship, &fit).await.ok().unwrap().scalars();
        assert!(restored.0 < large);
        assert!((restored.1 - armor).abs() < 1e-9 && (restored.2 - hull).abs() < 1e-9);
        let kwargs = Value::Dict(vec![]);
        let call = crate::services::Call { service: "dogmaIM", method: "ShipGetInfo", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let Value::Dict(info) = crate::services::dogma::dispatch(&mut f.client, &call).await.ok().unwrap() else { panic!("ship information") };
        let (_, Value::Instance(_, fields)) = info.iter().find(|(key, _)| *key == Value::Int(ship)).expect("ship information row") else { panic!("ship information instance") };
        let Some(Value::Dict(attributes)) = fields.get("attributes") else { panic!("ship attributes") };
        let charge = attributes.iter().find(|(key, _)| *key == Value::Int(i64::from(infinity_core::consts::dogma::ATTR_CHARGE))).expect("station capacitor");
        assert_eq!(charge.1, Value::Float(fit.value(Key::Ship, infinity_core::consts::dogma::ATTR_CAPACITOR_CAPACITY)));
        assert!(events.iter().all(|(name, _)| name != "OnGodmaPrimeLocation"));
    });
    let result = task.await;
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}
