//! Repackaging contents, batch, and refusal scenarios against the real database.
use super::*;
use infinity_core::consts::dogma::EFFECT_ONLINE;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_DRONE, FLAG_CARGO, FLAG_DRONE_BAY, FLAG_HI_SLOT_FIRST, FLAG_SUBSYSTEM_FIRST};
use tokio::sync::mpsc::UnboundedReceiver;

type Events = UnboundedReceiver<(String, Vec<Value>)>;

async fn insert(db: &Db, owner: i64, ty: i32, location: i64, flag: i32, quantity: i64, singleton: bool, online: bool) -> i64 {
    db.query_one("INSERT INTO game.items (owner_id,type_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING item_id", &[&owner,&ty,&location,&flag,&quantity,&singleton,&online]).await.unwrap().get("item_id")
}

pub(super) async fn many(c: &mut Client, station: i64, ids: &[i64], confirmation: Option<Value>) -> Result<Value, CallError> {
    let args = [Value::Dict(vec![(Value::Int(station), Value::list(ids.iter().map(|&id| Value::tuple([Value::Int(id), Value::Int(station)]))))])];
    let kwargs = Value::Dict(confirmation.into_iter().map(|v| (Value::str("destroyable"), v)).collect());
    let call = Call { service: "repackagingSvc", method: "RepackageItems", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    super::super::dispatch(c, &call).await.expect("repackage dispatch")
}

async fn unchanged(c: &mut Client, db: &Db, f: Fixture, id: i64, events: &mut Events) {
    let before = fingerprint(db).await;
    assert!(many(c, f.station, &[id], Some(Value::Bool(true))).await.is_err());
    assert_eq!(fingerprint(db).await, before);
    assert!(events.try_recv().is_err());
}

pub(super) async fn sealed_wrap(c: &mut Client, db: &Db, f: Fixture, rig_type: i32, events: &mut Events) {
    let ordinary = insert(db, f.owner, rig_type, f.station, FLAG_HANGAR, 1, true, false).await;
    let wrap = insert(db, f.owner, infinity_core::consts::TYPE_PLASTIC_WRAP, f.station, FLAG_HANGAR, -2, true, false).await;
    let child = insert(db, f.owner, rig_type, wrap, FLAG_CARGO, 3, false, false).await;
    assert!(ordinary < wrap);
    let before = fingerprint(db).await;
    for ids in [&[wrap][..], &[ordinary, wrap][..]] {
        let result = many(c, f.station, ids, Some(Value::Bool(true))).await;
        assert!(matches!(result, Err(CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        assert_eq!(fingerprint(db).await, before);
        assert!(events.try_recv().is_err());
        assert!(c.outbox.is_empty());
        let row = db.query_one("SELECT quantity,singleton FROM game.items WHERE item_id=$1", &[&wrap]).await.unwrap();
        assert_eq!(row.get::<_, i64>("quantity"), -2);
        assert!(row.get::<_, bool>("singleton"));
        let row = db.query_one("SELECT quantity,singleton FROM game.items WHERE item_id=$1", &[&ordinary]).await.unwrap();
        assert_eq!(row.get::<_, i64>("quantity"), 1);
        assert!(row.get::<_, bool>("singleton"));
        let row = db.query_one("SELECT location_id,quantity,singleton FROM game.items WHERE item_id=$1", &[&child]).await.unwrap();
        assert_eq!(row.get::<_, i64>("location_id"), wrap);
        assert_eq!(row.get::<_, i64>("quantity"), 3);
        assert!(!row.get::<_, bool>("singleton"));
    }
}

async fn contents(c: &mut Client, db: &Db, f: Fixture, hull_type: i32, rig_type: i32, events: &mut Events) {
    let module_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE) && t.effects().iter().any(|e| e.effect_id == 12)).expect("online high-slot module").type_id;
    let charge_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_CHARGE) && crate::services::item_volume::item_volume(&c.sde, t.type_id, false, 1, None).is_ok_and(|v| v.is_finite() && v > 0.0)).expect("charge with positive volume").type_id;
    let drone_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).expect("drone").type_id;
    let subsystem_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SUBSYSTEM)).expect("subsystem").type_id;
    let container_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER) && crate::services::item_volume::item_volume(&c.sde, t.type_id, true, 1, None).is_ok_and(|v| v.is_finite() && v > 0.0)).expect("cargo container with positive volume").type_id;
    let ship = insert(db, f.owner, hull_type, f.station, FLAG_HANGAR, 1, true, false).await;
    let module = insert(db, f.owner, module_type, ship, FLAG_HI_SLOT_FIRST, 1, true, true).await;
    let fitted_rig = insert(db, f.owner, rig_type, ship, FLAG_RIG_FIRST, 1, true, false).await;
    let subsystem = insert(db, f.owner, subsystem_type, ship, FLAG_SUBSYSTEM_FIRST, 1, true, false).await;
    let cargo = insert(db, f.owner, rig_type, ship, FLAG_CARGO, 7, false, false).await;
    let drone = insert(db, f.owner, drone_type, ship, FLAG_DRONE_BAY, 3, false, false).await;
    let container = insert(db, f.owner, container_type, ship, FLAG_CARGO, 1, true, false).await;
    let nested = insert(db, f.owner, charge_type, container, FLAG_CARGO, 9, false, false).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,1,42)", &[&module]).await.unwrap();
    db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,5),($1,$4,$3,0)", &[&ship,&FLAG_HI_SLOT_FIRST,&charge_type,&(FLAG_HI_SLOT_FIRST + 1)]).await.unwrap();
    crate::dao::items::pending_reloads::insert(db, f.owner, ship, module, FLAG_HI_SLOT_FIRST, charge_type, 6, FLAG_CARGO, ship).await.unwrap();
    db.execute("INSERT INTO game.weapon_banks (ship_id,master_id,slave_id) VALUES ($1,$2,$3)", &[&ship,&module,&subsystem]).await.unwrap();
    let volume = |ty, singleton, quantity| crate::services::item_volume::item_volume(&c.sde, ty, singleton, quantity, None).unwrap();
    let cargo_volume = volume(rig_type, false, 7) + volume(container_type, true, 1);
    let recovered_volume = volume(charge_type, false, 6);
    assert!(cargo_volume.is_finite() && cargo_volume > 0.0 && recovered_volume.is_finite() && recovered_volume > 0.0);
    let capacity_attribute = infinity_core::consts::dogma::ATTR_CAPACITY;
    let total_recovered_volume = volume(charge_type, false, 11);
    assert!(crate::dao::items::dogma::set_override(db, ship, capacity_attribute, cargo_volume + 2.0 * recovered_volume).await.unwrap());
    let fit = crate::services::fit::Fit::load_unpiloted(db, c.sde.clone(), ship).await.unwrap();
    let capacity = crate::services::inventory::holds::effective_capacity(&fit, FLAG_CARGO);
    assert!(capacity >= cargo_volume + total_recovered_volume, "valid fixture capacity {capacity}, cargo {cargo_volume}, recovery {total_recovered_volume}");
    let before = fingerprint(db).await;
    for confirmation in [None, Some(Value::Bool(false)), Some(Value::str("true"))] {
        assert!(matches!(many(c, f.station, &[ship], confirmation).await, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES));
        assert_eq!(fingerprint(db).await, before);
        assert!(events.try_recv().is_err());
    }
    let result = many(c, f.station, &[ship], Some(Value::Bool(true))).await;
    assert!(matches!(&result, Ok(Value::None)), "repackage failed: {result:?}");
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&fitted_rig]).await.unwrap().is_none());
    for (id, qty) in [(module,1), (subsystem,1), (cargo,7), (drone,3), (container,1)] {
        let row = db.query_one("SELECT location_id,flag_id,quantity,online FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
        assert_eq!(row.get::<_, i64>("location_id"), f.station);
        assert_eq!(row.get::<_, i32>("flag_id"), FLAG_HANGAR);
        assert_eq!(row.get::<_, i64>("quantity"), qty);
        assert!(!row.get::<_, bool>("online"));
    }
    assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&nested]).await.unwrap().get::<_, i64>("location_id"), container);
    assert!(db.query_opt("SELECT item_id FROM game.item_attributes WHERE item_id=$1", &[&module]).await.unwrap().is_some());
    let quantity: i64 = db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.owner,&f.station,&FLAG_HANGAR,&charge_type]).await.unwrap().get("quantity");
    assert_eq!(quantity, 11);
    for table in ["module_charges", "pending_reloads", "weapon_banks"] {
        assert!(db.query(&format!("SELECT ship_id FROM game.{table} WHERE ship_id=$1"), &[&ship]).await.unwrap().is_empty());
    }
    let moved = crate::services::items::load_items(db, &c.sde, crate::dao::items::loading::ItemQuery::Id(module)).await.unwrap().pop().unwrap();
    let expected = Value::Dict(vec![(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(ship)), (Value::Int(infinity_core::consts::IX_FLAG), Value::Int(i64::from(FLAG_HI_SLOT_FIRST)))]);
    let mut module_notice = false;
    let mut charge_notice = false;
    while let Ok((name, payload)) = events.try_recv() {
        if name == "OnItemChange" && payload.first() == Some(&moved.row()) && payload.get(1) == Some(&expected) { module_notice = true; }
        if name == "OnModuleAttributeChanges" {
            if let Some(Value::List(rows)) = payload.first() {
                charge_notice |= rows.iter().any(|row| matches!(row, Value::Tuple(fields) if fields.get(1) == Some(&Value::Int(f.owner)) && fields.get(3) == Some(&Value::Int(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY))) && fields.get(5) == Some(&Value::Int(0)) && fields.get(6) == Some(&Value::Int(5))));
            }
        }
    }
    assert!(module_notice && charge_notice);
}

async fn batch_and_strategic(c: &mut Client, db: &Db, f: Fixture, hull_type: i32, rig_type: i32, events: &mut Events) {
    let strategic_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.group_id.map(i64::from) == Some(GROUP_STRATEGIC_CRUISER)).expect("strategic cruiser").type_id;
    let strategic = insert(db, f.owner, strategic_type, f.station, FLAG_HANGAR, 1, true, false).await;
    let rig = insert(db, f.owner, rig_type, strategic, FLAG_RIG_FIRST, 1, true, false).await;
    let empty = insert(db, f.owner, hull_type, f.station, FLAG_HANGAR, 1, true, false).await;
    let module = insert(db, f.owner, rig_type, f.station, FLAG_HANGAR, 1, true, false).await;
    let stack = insert(db, f.owner, rig_type, f.station, FLAG_HANGAR, 4, false, false).await;
    let before_empty = crate::services::items::load_items(db, &c.sde, crate::dao::items::loading::ItemQuery::Id(empty)).await.unwrap().pop().unwrap();
    assert!(matches!(many(c, f.station, &[strategic, empty, module, empty, stack], None).await, Ok(Value::None)));
    for id in [strategic, empty, module, stack] {
        assert!(!db.query_one("SELECT singleton FROM game.items WHERE item_id=$1", &[&id]).await.unwrap().get::<_, bool>("singleton"));
    }
    let row = db.query_one("SELECT location_id,flag_id FROM game.items WHERE item_id=$1", &[&rig]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), f.station);
    assert_eq!(row.get::<_, i32>("flag_id"), FLAG_HANGAR);
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap().get::<_, i64>("quantity"), 4);
    let after_empty = crate::services::items::load_items(db, &c.sde, crate::dao::items::loading::ItemQuery::Id(empty)).await.unwrap().pop().unwrap();
    let empty_row = after_empty.row();
    let stack_row = crate::services::items::load_items(db, &c.sde, crate::dao::items::loading::ItemQuery::Id(stack)).await.unwrap().pop().unwrap().row();
    let mut empty_changes = 0;
    let mut stack_changes = 0;
    let mut captured = None;
    let mut sequence = 0;
    while let Ok((name, payload)) = events.try_recv() {
        if name == "OnItemChange" && payload.first() == Some(&stack_row) { stack_changes += 1; }
        if name == "OnItemChange" && payload.first() == Some(&empty_row) {
            empty_changes += 1;
            captured = Some((sequence, name, payload));
        }
        sequence += 1;
    }
    assert_eq!(empty_changes, 1);
    assert_eq!(stack_changes, 0);
    let (sequence, name, payload) = captured.unwrap();
    let value = Value::tuple(payload);
    let wire = marshal::dumps(&value);
    let decoded = marshal::loads(&wire).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(decoded.items().unwrap().first(), Some(&empty_row));
    let wire_hex: String = wire.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
        "version": 1, "id": "repackage-singleton-change", "scenario": "repackage",
        "producer_test": "services::fitting::repackaging_svc::tests::rig_confirmation_preserves_then_destroys",
        "producer_symbols": ["repackagingSvc::RepackageItems", "batch_and_strategic"],
        "kind": "notification", "sequence": sequence, "event": name,
        "recipient": {"character_id": f.owner.to_string()},
        "session": {"source": "fixture_context", "character_id": f.owner.to_string(), "station_id": f.station.to_string()},
        "payload_scope": "notification_args", "wire_hex": wire_hex, "wire_size": wire.len(), "rows": [],
        "before": {"source": "fixture_context", "item_id": before_empty.item_id.to_string(), "quantity": before_empty.quantity.to_string(), "singleton": before_empty.singleton},
        "after": {"source": "server_output", "item_id": after_empty.item_id.to_string(), "quantity": after_empty.quantity.to_string(), "singleton": after_empty.singleton}
    }));
    let ordinary = insert(db, f.owner, hull_type, f.station, FLAG_HANGAR, 1, true, false).await;
    insert(db, f.owner, rig_type, ordinary, FLAG_RIG_FIRST, 1, true, false).await;
    assert!(matches!(many(c, f.station, &[ordinary], Some(Value::Int(1))).await, Ok(Value::None)));
    while events.try_recv().is_ok() {}
}

async fn guards(c: &mut Client, db: &Db, f: Fixture, hull_type: i32, rig_type: i32, events: &mut Events) {
    let ship = insert(db, f.owner, hull_type, f.station, FLAG_HANGAR, 1, true, false).await;
    c.session.apply(vec![("shipid", Value::Int(ship))]);
    let before = fingerprint(db).await;
    assert!(matches!(many(c, f.station, &[ship], None).await, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CANT_REPACKAGE_ACTIVE_SHIP));
    assert_eq!(fingerprint(db).await, before);
    assert!(events.try_recv().is_err());
    c.session.apply(vec![("shipid", Value::Int(0))]);
    c.session.apply(vec![("charid", Value::Int(0))]);
    unchanged(c, db, f, ship, events).await;
    c.session.apply(vec![("charid", Value::Int(f.owner))]);
    db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&ship,&FLAG_CARGO]).await.unwrap();
    unchanged(c, db, f, ship, events).await;
    db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&ship,&FLAG_HANGAR]).await.unwrap();
    let before = fingerprint(db).await;
    assert!(many(c, f.station + 1, &[ship], None).await.is_err());
    assert_eq!(fingerprint(db).await, before);
    assert!(events.try_recv().is_err());
    let args = [Value::Dict(vec![(Value::Int(f.station), Value::list([Value::tuple([Value::Int(ship), Value::Int(f.station + 1)])]))])];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "repackagingSvc", method: "RepackageItems", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(super::super::dispatch(c, &call).await.unwrap().is_err());
    assert_eq!(fingerprint(db).await, before);
    assert!(events.try_recv().is_err());
    let foreign = insert(db, -f.owner, rig_type, ship, FLAG_CARGO, 1, false, false).await;
    unchanged(c, db, f, ship, events).await;
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&foreign]).await.unwrap();
    let unknown_type = (1..=i32::MAX).rev().find(|&id| c.sde.types.get(id).is_none()).unwrap();
    let unknown = insert(db, f.owner, unknown_type, ship, FLAG_CARGO, 1, false, false).await;
    unchanged(c, db, f, ship, events).await;
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&unknown]).await.unwrap();
    let rig = insert(db, f.owner, rig_type, ship, FLAG_RIG_FIRST, 1, true, false).await;
    let nested = insert(db, f.owner, rig_type, rig, FLAG_CARGO, 1, false, false).await;
    unchanged(c, db, f, ship, events).await;
    db.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&&[nested,rig][..]]).await.unwrap();
    let trade_container = insert(db, f.owner, rig_type, ship, FLAG_CARGO, 1, true, false).await;
    db.execute("INSERT INTO game.trades (trade_id,station_id,char_a,char_b,state,created_at,updated_at) VALUES ($1,$2,$3,0,'open',0,0)", &[&trade_container,&f.station,&f.owner]).await.unwrap();
    unchanged(c, db, f, ship, events).await;
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade_container]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&trade_container]).await.unwrap();
    db.execute("INSERT INTO game.industry_jobs (installer_id,owner_id,activity_id,blueprint_id,blueprint_type_id,blueprint_copy,facility_id,solar_system_id,runs,cost,time_seconds,start_date,end_date,status) VALUES ($1,$1,1,$2,$3,false,$4,0,1,0,1,0,1,$5)", &[&f.owner,&ship,&hull_type,&f.station,&infinity_core::consts::INDUSTRY_JOB_STATUS_INSTALLED]).await.unwrap();
    unchanged(c, db, f, ship, events).await;
    db.execute("DELETE FROM game.industry_jobs WHERE owner_id=$1", &[&f.owner]).await.unwrap();
}

pub(super) async fn exercise(c: &mut Client, db: &Db, f: Fixture, hull_type: i32, rig_type: i32, events: &mut Events) {
    contents(c, db, f, hull_type, rig_type, events).await;
    batch_and_strategic(c, db, f, hull_type, rig_type, events).await;
    guards(c, db, f, hull_type, rig_type, events).await;
}
