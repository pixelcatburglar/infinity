use super::*;

mod database;
#[test]
fn update_payload_uses_none_for_expiry_cleanup() {
    let rows = vec![dao::ExpertSystem { character_id: 7, type_id: 42, installed_at: 10, expires_at: 20 }];
    let mut outbox: Outbox = Outbox::default();
    changed(&mut outbox, 7, &rows, false, 0);
    let entries = outbox.into_entries();
    let [crate::db::outbox::Effect::Notification(Notification::Pilot { event, args, .. })] = entries.as_slice() else { panic!("missing expert update notification") };
    assert_eq!(event, EVENT);
    assert_eq!(args[1], Value::Bool(false));
    assert_eq!(args[2], Value::None);
}

#[test]
fn update_payload_preserves_added_type() {
    let mut outbox: Outbox = Outbox::default();
    changed(&mut outbox, 7, &[], true, 42);
    let entries = outbox.into_entries();
    let [crate::db::outbox::Effect::Notification(Notification::Pilot { args, .. })] = entries.as_slice() else { panic!("missing expert update notification") };
    assert_eq!(args[1], Value::Bool(true));
    assert_eq!(args[2], Value::Int(42));
}
