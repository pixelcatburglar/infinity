use super::*;

#[test]
fn capacity_refusal_is_a_custom_user_error() {
    crate::tests::init_config();
    let error = refusal(1);
    assert!(is_refusal(&error));
    assert!(!is_refusal(&user_error(infinity_core::messages::MSG_CUSTOM_NOTIFY,
        vec![("notify".into(), Value::unicode(infinity_core::messages::effects::CLOAK_TOO_CLOSE))])));
    assert!(matches!(error, CallError::User { msg, dict }
        if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY
            && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::effects::COMBAT_CAPACITY_FULL))]));
}

#[test]
fn warnings_are_limited_without_sleeping() {
    let now = Instant::now();
    let interval = Duration::from_secs(30);
    let mut last = None;
    assert!(warning_due(&mut last, now, interval));
    assert!(!warning_due(&mut last, now + interval - Duration::from_secs(1), interval));
    assert!(warning_due(&mut last, now + interval, interval));
}
