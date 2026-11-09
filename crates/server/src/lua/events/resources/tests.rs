use super::*;

#[test]
fn loyalty_reasons_round_trip_and_project_to_lua() {
    let lua = Lua::new();
    for (reason, native, lua_label) in [
        (LoyaltyReason::MissionReward, "MissionReward", "mission_reward"),
        (LoyaltyReason::GmAdjustment, "GmAdjustment", "gm_adjustment"),
        (LoyaltyReason::StorePurchase, "StorePurchase", "store_purchase"),
        (LoyaltyReason::ChipExchange, "ChipExchange", "chip_exchange"),
        (LoyaltyReason::DailyReward, "DailyReward", "daily_reward"),
        (LoyaltyReason::DailyCompletion, "DailyCompletion", "daily_completion"),
    ] {
        let encoded = serde_json::to_string(&reason).unwrap();
        assert_eq!(encoded, format!("\"{native}\""));
        assert_eq!(serde_json::from_str::<LoyaltyReason>(&encoded).unwrap(), reason);
        let table = lua.create_table().unwrap();
        set_field(&lua, &table, "reason", &reason).unwrap();
        assert_eq!(table.get::<String>("reason").unwrap(), lua_label);
    }
}
