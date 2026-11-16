use super::*;
use crate::dao::items::loading::Item;
use crate::dao::repairable_ship::{ItemId, RecoveryGroupId, RecoveryItemId, SourceKind, SourceShape, Status};
use infinity_core::consts::{FILETIME_SECOND, FILETIME_UNIX_EPOCH, FLAG_HANGAR};
use prost::Message;
use std::collections::BTreeMap;

#[ignore = "requires capture source metadata environment"]
#[test]
fn repairable_client_capture_v1() {
    let character = 90_000_001;
    let old = ItemId::new(1_000_000_000_001).unwrap();
    let new = ItemId::new(1_000_000_000_002).unwrap();
    let group_id = RecoveryGroupId::new(10_000_000_000_001).unwrap();
    let member_id = RecoveryItemId::new(10_000_000_000_002).unwrap();
    let time = FILETIME_UNIX_EPOCH + 1_700_000_000 * FILETIME_SECOND;
    let shape = SourceShape { item: old, owner: character, type_id: 587, location: 30_000_001,
        flag: FLAG_HANGAR, quantity: 1, singleton: true, x: None, y: None, z: None,
        brain_slot: None, jump_clone: None };
    let group = Group { id: group_id, beneficiary: character, kind: SourceKind::PilotedShip,
        shape, system: 30_000_001, occurred_at: time, position: [None; 3], loss_id: None,
        destination_station: 60_000_001, payment_expires_at: time + 100 * FILETIME_SECOND };
    let member = RecoveryItem { id: member_id, group: group_id, item: old, original_owner: character,
        type_id: 587, flag: FLAG_HANGAR, quantity: 1, singleton: true, last_service_life: 80.0,
        repair_completed_at: time, delivery_completed_at: None,
        withdraw_expires_at: None, status: Status::AwaitingPayment, withdrawn_item: None };
    let other = RecoveryItem { id: RecoveryItemId::new(10_000_000_000_003).unwrap(),
        item: ItemId::new(1_000_000_000_003).unwrap(), ..member.clone() };
    let initial = Selection { group: group.clone(), items: vec![member.clone(), other.clone()] };
    let selected = Selection { group: group.clone(), items: vec![member.clone()] };
    let delivered = RecoveryItem { delivery_completed_at: Some(time + FILETIME_SECOND),
        withdraw_expires_at: Some(time + 2 * FILETIME_SECOND), status: Status::Delivering, ..member.clone() };
    let delivery = Selection { group: group.clone(), items: vec![delivered.clone()] };
    let withdrawn = Selection { group: group.clone(), items: vec![RecoveryItem {
        status: Status::Withdrawn, withdrawn_item: Some(new), ..delivered }] };
    let texts = [
        ("responsed-empty", "eve.repairableship.Responsed", responsed(character, &[]).unwrap()),
        ("exploded", "eve.repairableship.event.ExplodedResponsed", exploded(character, &initial).unwrap()),
        ("delivery-started", "eve.repairableship.DeliveryStarted", delivery_started(character, &[delivery]).unwrap()),
        ("abort-started", "eve.repairableship.AbortStarted", abort_started(character, &[selected]).unwrap()),
        ("items-withdrawn", "eve.repairableship.ItemsWithdrawn", items_withdrawn(character, &[withdrawn]).unwrap()),
    ];
    let damage = BTreeMap::from([(old, 0.0)]);
    let facts = serde_json::json!({"group_id": group_id.get().to_string(), "member_id": member_id.get().to_string(),
        "other_member_id": other.id.get().to_string(), "other_original_item_id": other.item.get().to_string(),
        "original_item_id": old.get().to_string(), "withdrawn_item_id": new.get().to_string(),
        "ship_type": group.shape.type_id, "item_type": member.type_id, "quantity": member.quantity,
        "owner_id": character.to_string(), "flag_id": member.flag, "singleton": member.singleton,
        "damage": damage[&old], "last_service_life": member.last_service_life,
        "station": group.destination_station.to_string(), "solar_system": group.system.to_string(),
        "explosion_time": unix_parts_from_filetime(time), "delivery_time": unix_parts_from_filetime(time + FILETIME_SECOND),
        "withdraw_time": unix_parts_from_filetime(time + 2 * FILETIME_SECOND) });
    let mut records = Vec::new();
    for (id, name, text) in texts {
        let descriptor = crate::gateway::message::message(name).unwrap();
        let parsed = DynamicMessage::parse_text_format(descriptor.clone(), &text).unwrap();
        let decoded = DynamicMessage::decode(descriptor, parsed.encode_to_vec().as_slice()).unwrap();
        assert_eq!(parsed, decoded);
        if id == "responsed-empty" {
            assert!(matches!(parsed.get_field_by_name("recovery_groups").unwrap().as_ref(), PV::List(rows) if rows.is_empty()));
        }
        if id == "exploded" {
            let group_value = parsed.get_field_by_name("recovery_group").unwrap();
            let PV::Message(group_message) = group_value.as_ref() else { panic!("missing recovery group") };
            let id_value = group_message.get_field_by_name("id").unwrap();
            let PV::Message(identifier) = id_value.as_ref() else { panic!("missing group identity") };
            assert_eq!(identifier.get_field_by_name("sequential").unwrap().as_ref(), &PV::U64(group_id.get() as u64));
            let items = group_message.get_field_by_name("recovery_items").unwrap();
            assert!(matches!(items.as_ref(), PV::List(rows) if rows.len() == 2));
        }
        records.push(serde_json::json!({"id":id, "kind":"text_notification", "scenario":id,
            "payload_scope":"production_encoder", "message_type":name, "text":text,
            "length":text.len(), "sha256":crate::hash::sha256_hex(text.as_bytes()), "facts":facts}));
    }
    let life = super::super::service_life::damage_dict(&damage);
    let row = Item { item_id: old.get(), type_id: member.type_id, owner_id: character,
        location_id: group.destination_station, flag_id: FLAG_HANGAR, quantity: 1, singleton: true,
        group_id: 1, category_id: 1, custom_info: None }.row();
    let input = marshal::Value::List(vec![row.clone()]);
    let result = super::super::refurbishment_result(&[(old.get(), row)], &damage);
    let encode = |value: &marshal::Value| {
        let bytes = marshal::dumps(value);
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        serde_json::json!({"hex":hex, "length":bytes.len(), "sha256":crate::hash::sha256_hex(&bytes)})
    };
    records.push(serde_json::json!({"id":"life-damage-changed", "kind":"macho_dict", "scenario":"life-damage-changed",
        "payload_scope":"production_encoder", "payload":encode(&life), "facts":facts}));
    records.push(serde_json::json!({"id":"refurbishment-result", "kind":"macho_value", "scenario":"refurbishment-result",
        "payload_scope":"production_encoder", "input":encode(&input), "result":encode(&result), "facts":facts}));
    let source = serde_json::json!({"base_sha":std::env::var("REPAIRABLE_CAPTURE_BASE_SHA").unwrap(),
        "head_sha":std::env::var("REPAIRABLE_CAPTURE_HEAD_SHA").unwrap(), "dirty":std::env::var("REPAIRABLE_CAPTURE_DIRTY").unwrap().parse::<bool>().unwrap(),
        "wip_summary":format!("production_encoder: {}", std::env::var("REPAIRABLE_CAPTURE_WIP_SUMMARY").unwrap()), "producer_command":std::env::var("REPAIRABLE_CAPTURE_COMMAND").unwrap()});
    println!("REPAIRABLE_CLIENT_CAPTURE_V1 {}", serde_json::json!({"consumer":"repairable_ship_lifecycle", "source":source, "records":records}));
}
