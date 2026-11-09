use super::*;
use crate::space::combat_facts::Refusal;

#[test]
fn capacity_refusal_is_user_visible_but_replay_is_benign() {
    crate::tests::init_config();
    assert!(matches!(fact_refusal(1, Refusal::Full), Some(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY));
    assert!(fact_refusal(1, Refusal::Replay).is_none());
    assert!(matches!(fact_refusal(1, Refusal::Overflow), Some(CallError::Internal(_))));
}
