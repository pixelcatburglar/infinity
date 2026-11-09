use super::*;
use crate::lua::events::resources::{FactContext, FactOrigin};

fn context(origin: FactOrigin) -> FactContext { FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin } }
fn completion(category: GoalCategory, period: GoalPeriod, reason: CompletionReason, origin: FactOrigin) -> GameEvent {
    GameEvent::DailyGoalCompleted { char_id: 1, goal_id: "00000000-0000-0000-0000-000000000001".into(), category, period, period_start: 0, period_end: 10, paid: reason == CompletionReason::Paid, reason, occurred_at: 11, context: context(origin) }
}
#[test]
fn completion_enums_codec_and_lua_round_trip() {
    for (category, period) in [(GoalCategory::Ordinary, GoalPeriod::Day), (GoalCategory::Bonus, GoalPeriod::Day), (GoalCategory::Month, GoalPeriod::Month)] {
        for (reason, origin) in [(CompletionReason::Natural, FactOrigin::Natural), (CompletionReason::Paid, FactOrigin::Natural), (CompletionReason::Qa, FactOrigin::Qa), (CompletionReason::Derived, FactOrigin::Natural), (CompletionReason::Derived, FactOrigin::Qa)] {
            let event = completion(category, period, reason, origin);
            let payload = crate::progress::codec::encode(&event).unwrap();
            assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, event.kind(), &payload).unwrap(), event);
            let lua = Lua::new(); let table = event.to_lua(&lua).unwrap();
            assert_eq!(table.get::<String>("category").unwrap(), format!("{category:?}"));
            assert_eq!(table.get::<String>("period").unwrap(), format!("{period:?}"));
            assert_eq!(table.get::<String>("reason").unwrap(), format!("{reason:?}"));
            assert_eq!(table.get::<i64>("period_start").unwrap(), 0);
            assert_eq!(table.get::<String>("goal_id").unwrap(), "00000000-0000-0000-0000-000000000001");
        }
    }
}
#[test]
fn invalid_completion_and_reward_are_rejected() {
    let mut event = completion(GoalCategory::Month, GoalPeriod::Day, CompletionReason::Natural, FactOrigin::Natural);
    assert!(validate(&event).is_err());
    event = completion(GoalCategory::Ordinary, GoalPeriod::Day, CompletionReason::Qa, FactOrigin::Natural);
    assert!(validate(&event).is_err());
    event = completion(GoalCategory::Ordinary, GoalPeriod::Day, CompletionReason::Natural, FactOrigin::Natural);
    if let GameEvent::DailyGoalCompleted { goal_id, .. } = &mut event { *goal_id = "BAD".into(); }
    assert!(validate(&event).is_err());
    let mut reward = GameEvent::EverMarksEarned { char_id: 1, goal_id: "00000000-0000-0000-0000-000000000001".into(), reward_index: 0, asset_id: "00000000-0000-0000-0000-000000000002".into(), issuer_id: infinity_core::consts::CORP_HERALDRY, amount: 1, reason: EverMarkReason::DailyReward, occurred_at: 1, context: context(FactOrigin::Natural) };
    let payload = crate::progress::codec::encode(&reward).unwrap();
    assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, reward.kind(), &payload).unwrap(), reward);
    let lua = Lua::new(); assert_eq!(reward.to_lua(&lua).unwrap().get::<String>("reason").unwrap(), "DailyReward");
    if let GameEvent::EverMarksEarned { amount, .. } = &mut reward { *amount = 0; }
    assert!(validate(&reward).is_err());
}

#[test]
fn malformed_fields_are_rejected_on_both_codec_paths() {
    let event = completion(GoalCategory::Ordinary, GoalPeriod::Day, CompletionReason::Natural, FactOrigin::Natural);
    for (field, value) in [
        ("char_id", serde_json::json!(0)), ("goal_id", serde_json::json!("00000000-0000-0000-0000-00000000000A")),
        ("period_start", serde_json::json!(-1)), ("period_end", serde_json::json!(0)),
        ("occurred_at", serde_json::json!(0)), ("paid", serde_json::json!(true)),
        ("period", serde_json::json!("Month")),
    ] {
        let mut value_event = serde_json::to_value(&event).unwrap();
        value_event["DailyGoalCompleted"][field] = value;
        let event: GameEvent = serde_json::from_value(value_event).unwrap();
        assert!(crate::progress::codec::encode(&event).is_err(), "{field}");
        assert!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, event.kind(), &serde_json::to_string(&event).unwrap()).is_err(), "{field}");
    }
    let mut event = completion(GoalCategory::Ordinary, GoalPeriod::Day, CompletionReason::Natural, FactOrigin::Natural);
    if let GameEvent::DailyGoalCompleted { context, .. } = &mut event { context.system_id = Some(0); }
    assert!(validate(&event).is_err());
    if let GameEvent::DailyGoalCompleted { context, .. } = &mut event { context.system_id = None; context.origin = FactOrigin::Gm; }
    assert!(validate(&event).is_err());
}

#[test]
fn reward_identity_and_origin_validation() {
    let event = GameEvent::EverMarksEarned { char_id: 1, goal_id: "00000000-0000-0000-0000-000000000001".into(), reward_index: 0, asset_id: "00000000-0000-0000-0000-000000000002".into(), issuer_id: infinity_core::consts::CORP_HERALDRY, amount: 2, reason: EverMarkReason::DailyReward, occurred_at: 1, context: context(FactOrigin::Natural) };
    for (field, value) in [
        ("char_id", serde_json::json!(0)), ("goal_id", serde_json::json!("bad")),
        ("reward_index", serde_json::json!(-1)), ("asset_id", serde_json::json!("bad")),
        ("issuer_id", serde_json::json!(0)), ("amount", serde_json::json!(0)),
        ("occurred_at", serde_json::json!(0)),
    ] {
        let mut json = serde_json::to_value(&event).unwrap();
        json["EverMarksEarned"][field] = value;
        let invalid: GameEvent = serde_json::from_value(json).unwrap();
        assert!(crate::progress::codec::encode(&invalid).is_err(), "{field}");
        assert!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, invalid.kind(), &serde_json::to_string(&invalid).unwrap()).is_err(), "{field}");
    }
    let lua = Lua::new(); let table = event.to_lua(&lua).unwrap();
    assert_eq!(table.get::<i64>("char_id").unwrap(), 1);
    assert_eq!(table.get::<i64>("reward_index").unwrap(), 0);
    assert_eq!(table.get::<String>("asset_id").unwrap(), "00000000-0000-0000-0000-000000000002");
    assert_eq!(table.get::<i64>("issuer_id").unwrap(), infinity_core::consts::CORP_HERALDRY);
    assert_eq!(table.get::<i64>("amount").unwrap(), 2);
    assert_eq!(table.get::<i64>("occurred_at").unwrap(), 1);
    let mut invalid = event;
    if let GameEvent::EverMarksEarned { context, .. } = &mut invalid { context.origin = FactOrigin::Qa; }
    assert!(validate(&invalid).is_err());
}
