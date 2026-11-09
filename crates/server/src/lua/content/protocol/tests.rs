use super::*;
use prost_reflect::ReflectMessage;

fn decode_content_text(kind: Message, text: &str) -> anyhow::Result<DynamicMessage> {
    Ok(DynamicMessage::parse_text_format(descriptor(kind)?, text)?)
}

#[test]
fn descriptor_and_zero_chip_text_round_trip() {
    validate().unwrap();
    let chips = [Chip { type_id: 1, quantity: 0 }, Chip { type_id: 2, quantity: 1 }];
    let text = encode_content_text(Response::User { identity: Identity { user: 1, character: 2 }, chips: &chips }).unwrap();
    let decoded = decode_content_text(Message::User, &text).unwrap();
    let values = decoded.get_field_by_name("chips").unwrap();
    let Value::List(values) = values.as_ref() else { panic!("chips must be repeated") };
    assert_eq!(values.len(), 2);
    let Value::Message(first) = &values[0] else { panic!("chip must be a message") };
    assert_eq!(first.get_field_by_name("quantity").unwrap().as_ref(), &Value::U32(0));
}

#[test]
fn inactive_mission_and_exchange_have_no_extra_fields() {
    let mission = encode_content_text(Response::Mission { identity: Identity { user: 1, character: 2 } }).unwrap();
    let message = decode_content_text(Message::Mission, &mission).unwrap();
    assert!(!message.has_field_by_name("active_mission"));
    let exchange = encode_content_text(Response::Exchange { identity: Identity { user: 1, character: 2 }, corporation: 3 }).unwrap();
    let message = decode_content_text(Message::Exchange, &exchange).unwrap();
    assert!(message.has_field_by_name("corporation"));
    assert!(message.descriptor().get_field_by_name("amount").is_none());
}

#[test]
fn tree_identity_and_identifier_ranges_are_checked() {
    let text = encode_content_text(Response::Tree { identity: Identity { user: 1, character: 2 }, tree: 3 }).unwrap();
    let decoded = decode_content_text(Message::Tree, &text).unwrap();
    assert!(decoded.has_field_by_name("tree"));
    assert!(encode_content_text(Response::Mission { identity: Identity { user: i64::from(i32::MAX) + 1, character: 2 } }).is_err());
    assert!(encode_content_text(Response::Exchange { identity: Identity { user: 1, character: 2 }, corporation: i64::from(u32::MAX) + 1 }).is_err());
}
