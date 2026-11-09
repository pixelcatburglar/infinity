use super::*;
use crate::lua::{content::daily_goals::model::{Career, Method, Reward, Template}, events::combat::BinaryAmount};

fn fixture() -> (GoalRow, GoalSnapshot) {
    let template = Template { category: Category::Ordinary, omega_required: false, key: "test".into(), target: 1, career: Career::None, method: Method::SpaceJump, rewards: vec![Reward::Evermark { amount: 2 }] };
    let snapshot = GoalSnapshot { template, messages: super::super::MessageRefs { name: 1, description: 1, help: 1 }, assigner: CORP_HERALDRY };
    let slot = "ordinary:0";
    let goal = GoalRow { goal_id: ids::goal(1, 0, "v", slot, 0), character_id: 1, kind: "day".into(), period_start: 0, period_end: 10, slot_key: slot.into(), generation: 0, namespace: "daily_goals".into(), version: "v".into(), category: "ordinary".into(), definition: serde_json::to_string(&snapshot).unwrap(), target: 1, progress: 1, remainder: BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: Some(11), earned_omega: false, paid: false, voided_at: None, has_unclaimed: true, revision: 1 };
    (goal, snapshot)
}
#[test]
fn builders_require_postwrite_rows_and_frozen_asset() {
    let (mut goal, snapshot) = fixture();
    let mut outbox = Outbox::default();
    completed(&mut outbox, &goal, &snapshot, CompletionReason::Natural, FactOrigin::Natural, 11).unwrap();
    goal.progress = 0;
    assert!(completed(&mut outbox, &goal, &snapshot, CompletionReason::Natural, FactOrigin::Natural, 11).is_err());
    goal.progress = 1;
    let mut reward = super::super::frozen::rewards(&goal, &snapshot.template).unwrap().remove(0);
    assert!(evermarks(&mut outbox, &goal, &reward, 12).is_err());
    reward.earned = true; reward.claimed_at = Some(12);
    evermarks(&mut outbox, &goal, &reward, 12).unwrap();
    reward.asset_id = goal.goal_id.clone();
    assert!(evermarks(&mut outbox, &goal, &reward, 12).is_err());
    let (_, events) = crate::db::outbox::tests::split(outbox);
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[1], GameEvent::EverMarksEarned { amount: 2, .. }));
}
