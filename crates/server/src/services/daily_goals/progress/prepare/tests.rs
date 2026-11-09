//! Canonical prepared-envelope identity checks.
use super::*;
use crate::lua::events::GameEvent;

#[test]
fn prepared_identity_rejects_changed_journal_metadata() {
    let mut event = EventEnvelope {
        event_id: "00000000-0000-0000-0000-000000000001".into(),
        event: GameEvent::Jumped { char_id: 1, ship_id: 2, from_system_id: 3, to_system_id: 4 },
        schema_version: 1, kind: "Jumped".into(), payload: "{}".into(),
        occurred_at: 5, recorded_at: 6, source_namespace: "business".into(),
        source_key: "Jumped:7".into(), ordinal: 0, context: "{}".into(),
        causation: None, origin: "server".into(), subject_character_id: Some(1),
    };
    let identity = Identity::new(&event);
    assert!(identity.matches(&event));
    event.subject_character_id = Some(2);
    assert!(!identity.matches(&event));
    event.subject_character_id = Some(1);
    event.payload = "{\"changed\":true}".into();
    assert!(!identity.matches(&event));
}
