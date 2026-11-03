use super::*;
use prost::Message;
use prost_reflect::Value;

fn event(url: &str, bytes: &[u8]) -> DynamicMessage {
    let mut event = DynamicMessage::new(message("eve_public.Event").unwrap());
    event.set_field_by_name("uuid", Value::Bytes(vec![1; UUID_LEN].into()));
    event.set_field_by_name("application_instance_uuid", Value::Bytes(vec![2; UUID_LEN].into()));
    let mut payload = DynamicMessage::new(message("google.protobuf.Any").unwrap());
    payload.set_field_by_name("type_url", Value::String(url.to_owned()));
    payload.set_field_by_name("value", Value::Bytes(bytes.to_vec().into()));
    event.set_field_by_name("payload", Value::Message(payload));
    event
}

#[test]
fn validates_envelopes_and_known_payloads() {
    let app = [2; UUID_LEN];
    let mut e = event("type.googleapis.com/eve_public.achievement.api.Pinned", &[]);
    assert!(receive(&e.encode_to_vec(), &app).is_ok());
    assert!(receive(&e.encode_to_vec(), &[3; UUID_LEN]).is_err());
    e.set_field_by_name("journey", Value::Bytes(vec![1].into()));
    assert!(receive(&e.encode_to_vec(), &app).is_err());
    assert!(receive(&event("type.googleapis.com/eve_public.achievement.api.Pinned", &[0xff]).encode_to_vec(), &app).is_err());
    assert!(receive(&event("type.googleapis.com/unknown.Event", &[]).encode_to_vec(), &app).is_ok());
    e.clear_field_by_name("payload");
    assert!(receive(&e.encode_to_vec(), &app).is_err());
}

#[test]
fn known_ui_events_are_decoded_without_consumers() {
    for name in [
        "eve_public.achievement.api.Pinned", "eve_public.achievement.api.Unpinned",
        "eve_public.achievement.api.Tracked", "eve_public.achievement.api.Untracked",
        "eve_public.app.eveonline.career.Selected", "eve_public.app.eveonline.career.goal.Selected",
        "eve_public.app.eveonline.career.goal.Tracked", "eve_public.app.eveonline.career.group.Selected",
        "eve_public.freelance.project.api.DiscoveryOverflowed", "eve_public.goal.api.CloneFormOpened",
    ] {
        assert!(message(name).is_some(), "{name}");
        assert!(receive(&event(&format!("type.googleapis.com/{name}"), &[]).encode_to_vec(), &[2; UUID_LEN]).is_ok(), "{name}");
    }
}

#[test]
fn validates_event_identity_journey_and_payload_presence() {
    let app = [2; UUID_LEN];
    let base = event("type.googleapis.com/eve_public.achievement.api.Pinned", &[]);
    for field in ["uuid", "application_instance_uuid"] {
        for bytes in [vec![], vec![2; UUID_LEN - 1], vec![2; UUID_LEN + 1]] {
            let mut invalid = base.clone();
            invalid.set_field_by_name(field, Value::Bytes(bytes.into()));
            assert!(receive(&invalid.encode_to_vec(), &app).is_err(), "{field}");
        }
    }
    let mut valid = base.clone();
    valid.set_field_by_name("journey", Value::Bytes(vec![1; UUID_LEN].into()));
    assert!(receive(&valid.encode_to_vec(), &app).is_ok());
    valid.set_field_by_name("journey", Value::Bytes(vec![1; UUID_LEN + 1].into()));
    assert!(receive(&valid.encode_to_vec(), &app).is_err());
    assert!(receive(&event("", &[]).encode_to_vec(), &app).is_err());
    assert!(receive(&[0xff], &app).is_err());
}
