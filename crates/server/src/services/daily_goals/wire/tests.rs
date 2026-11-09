//! Descriptor-backed projection checks using synthetic server-owned rows.
use super::*;
use prost::Message;
use crate::{dao::daily_goals::instances::GoalRow, lua::{content::{daily_goals::model::{Category, Career, Method, Template}, protocol::gateway}, events::combat::BinaryAmount}, services::daily_goals::{GoalSnapshot, MessageRefs}};

fn view() -> GoalView {
    let id = "00000000-0000-0000-0000-000000000001".to_string();
    GoalView {
        goal: GoalRow { goal_id: id, character_id: 1, kind: "day".into(), period_start: 0, period_end: 1, slot_key: "test".into(), generation: 0, namespace: "test".into(), version: "test".into(), category: "ordinary".into(), definition: "{}".into(), target: u64::MAX, progress: u64::MAX, remainder: BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: Some(1), earned_omega: false, paid: true, voided_at: None, has_unclaimed: false, revision: 0 },
        rewards: vec![], current_omega: false,
        snapshot: GoalSnapshot { template: Template { category: Category::Ordinary, omega_required: false, key: "test".into(), target: u64::MAX, career: Career::None, method: Method::SpaceJump, rewards: vec![] }, messages: MessageRefs { name: u32::MAX.into(), description: 1, help: 1 }, assigner: 1 },
    }
}
fn roundtrip(m: &DynamicMessage) -> DynamicMessage {
    let bytes = m.encode_to_vec();
    let decoded = DynamicMessage::decode(m.descriptor(), bytes.as_slice()).unwrap();
    let text = gateway::encode(&decoded, &decoded.descriptor()).unwrap();
    let parsed = DynamicMessage::parse_text_format(decoded.descriptor(), &text).unwrap();
    assert_eq!(decoded, parsed);
    decoded
}
#[test]
fn current_get_history_and_notices_roundtrip() {
    validate().unwrap();
    let v = view();
    let current = roundtrip(&current_response(std::slice::from_ref(&v)).unwrap());
    let goals = current.get_field_by_name("goals").unwrap();
    let goal = goals.as_list().unwrap()[0].as_message().unwrap();
    assert_eq!(goal.get_field_by_name("current_progress").unwrap().as_u64(), Some(u64::MAX));
    assert_eq!(goal.get_field_by_name("paid_completion").unwrap().as_bool(), Some(true));
    let get = roundtrip(&get_response(&v).unwrap());
    assert!(get.descriptor().get_field_by_name("paid_completion").is_none());
    assert_eq!(get.get_field_by_name("progress").unwrap().as_u64(), Some(u64::MAX));
    let history = roundtrip(&history_response(&HistoryPage { ids: vec![v.goal.goal_id.clone()], next_token: String::new() }).unwrap());
    assert_eq!(history.get_field_by_name("ids").unwrap().as_list().unwrap().len(), 1);
    assert!(current_notice(1, std::slice::from_ref(&v)).is_ok());
    assert!(progressed_notice(1, &v.goal.goal_id, u64::MAX).is_ok());
    assert!(completed_notice(1, &v.goal.goal_id).is_ok());
    assert!(redeemed_notice(1, &v.goal.goal_id).is_ok());
    assert!(progressed_notice(i64::MAX, &v.goal.goal_id, 1).is_err());
    assert!(empty_response("eve_public.dailygoal.api.RedeemAllRequest").is_err());
}

#[test]
fn all_contribution_configurations_roundtrip() {
    use crate::lua::content::daily_goals::model::{CombatEntry, CompletionEntry, LoyaltyEntry, ManufacturingEntry, Matcher as M, MiningEntry, Owner, Period, ScanEntry, SystemEntry, WarfareEntry};
    let system = || SystemEntry { system: M::Id { id: 1 } };
    let combat = || CombatEntry { system: M::Any, ship: M::Type { id: 1 }, identity: M::Alliance { id: 1 } };
    let warfare = || WarfareEntry { faction: M::Id { id: 1 }, system: M::Any, archetype: M::Id { id: 1 } };
    let methods = vec![
        Method::KillNpc { entries: vec![system(), SystemEntry { system: M::Any }] },
        Method::DamageShip { entries: vec![combat()] },
        Method::MineOre { entries: vec![MiningEntry { system: M::Any, ore: M::Type { id: 1 } }] },
        Method::CaptureFwComplex { entries: vec![warfare()] },
        Method::DefendFwComplex { entries: vec![warfare()] },
        Method::RemoteRepairArmor { entries: vec![combat()] },
        Method::RemoteRepairShields { entries: vec![combat()] },
        Method::ScanSignature { entries: vec![ScanEntry { system: M::Any, signature: M::Id { id: 1 } }] },
        Method::ManufactureItem { entries: vec![
            ManufacturingEntry { owner: Owner::Any, facility: M::Any, item: M::Any },
            ManufacturingEntry { owner: Owner::Character, facility: M::Station { id: 1 }, item: M::Type { id: 1 } },
            ManufacturingEntry { owner: Owner::Corporation, facility: M::Structure { id: 1 }, item: M::Any },
        ] },
        Method::CompleteGoal { entries: vec![CompletionEntry { goal: M::Goal { uuid: "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".into() }, category: M::Bonus, period: Period::Month }] },
        Method::SalvageWreck { entries: vec![system()] },
        Method::EarnLoyaltyPoints { entries: vec![LoyaltyEntry { corporation: M::Id { id: 1 } }] },
        Method::SpaceJump,
    ];
    for method in methods {
        let mut v = view();
        v.snapshot.template.method = method;
        roundtrip(&current_response(&[v]).unwrap());
    }
}

#[test]
fn invalid_wire_boundaries_fail() {
    let mut v = view();
    v.snapshot.messages.name = u64::MAX;
    assert!(get_response(&v).is_err());
    v.snapshot.messages.name = 1;
    v.goal.paid = true;
    v.goal.progress = 0;
    assert!(get_response(&v).is_err());
    assert!(set(&mut request("Get").unwrap(), "nonexistent", Value::Bool(false)).is_err());
}

fn reward(unit: &str) -> RewardRow {
    RewardRow { goal_id: "00000000-0000-0000-0000-000000000001".into(), reward_index: 0, asset_id: "00000000-0000-0000-0000-000000000002".into(), unit: unit.into(), amount_isk: None, amount_units: None, issuer_id: None, type_id: None, omega_required: false, earned: true, claimed_at: None, location_id: None, item_id: None, plex_reference: None, revision: 0 }
}

#[test]
fn five_reward_units_and_overflow() {
    use crate::isk::IskAmount;
    let mut rows = Vec::new();
    let mut isk = reward("isk"); isk.amount_isk = Some(IskAmount::parse_decimal("1.23").unwrap()); rows.push(isk);
    let mut lp = reward("lp"); lp.amount_units = Some(u64::MAX); lp.issuer_id = Some(1); rows.push(lp);
    let mut sp = reward("sp"); sp.amount_units = Some(u32::MAX.into()); rows.push(sp);
    let mut item = reward("item"); item.amount_units = Some(u32::MAX.into()); item.type_id = Some(1); rows.push(item);
    let mut plex = reward("plex"); plex.amount_units = Some(1); rows.push(plex);
    let mut v = view(); v.rewards = rows;
    let decoded = roundtrip(&get_response(&v).unwrap());
    assert_eq!(decoded.get_field_by_name("earnings").unwrap().as_list().unwrap().len(), 5);
    let goal = decoded.get_field_by_name("goal").unwrap();
    let payments = goal.as_message().unwrap().get_field_by_name("payment").unwrap();
    assert_eq!(payments.as_list().unwrap().len(), 5);
    for (row, payment) in v.rewards.iter().zip(payments.as_list().unwrap()) {
        let p = payment.as_message().unwrap();
        let asset = p.get_field_by_name("asset").unwrap();
        assert_eq!(asset.as_message().unwrap().get_field_by_name("uuid").unwrap().as_bytes().unwrap().len(), 16);
        let u = p.get_field_by_name("unit").unwrap();
        assert!(u.as_message().unwrap().has_field_by_name(&row.unit));
    }
    v.rewards[4].amount_units = Some(u64::MAX);
    assert!(get_response(&v).is_err());
    v.rewards[4].amount_units = Some(1);
    v.rewards[2].amount_units = Some(u64::MAX);
    assert!(get_response(&v).is_err());
}

#[test]
fn paid_and_omega_earnings_preserve_persisted_state() {
    let mut v = view();
    let mut sp = reward("sp"); sp.amount_units = Some(1); sp.omega_required = true;
    v.rewards.push(sp);
    let locked = get_response(&v).unwrap();
    let earning = locked.get_field_by_name("earnings").unwrap();
    assert_eq!(earning.as_list().unwrap()[0].as_message().unwrap().get_field_by_name("omega_required").unwrap().as_bool(), Some(true));
    v.goal.earned_omega = true;
    let unlocked = get_response(&v).unwrap();
    let earning = unlocked.get_field_by_name("earnings").unwrap();
    assert_eq!(earning.as_list().unwrap()[0].as_message().unwrap().get_field_by_name("omega_required").unwrap().as_bool(), Some(false));
    v.goal.earned_omega = false;
    v.current_omega = true;
    let unlocked = get_response(&v).unwrap();
    let earning = unlocked.get_field_by_name("earnings").unwrap();
    assert_eq!(earning.as_list().unwrap()[0].as_message().unwrap().get_field_by_name("omega_required").unwrap().as_bool(), Some(false));
    v.rewards[0].claimed_at = Some(1);
    let claimed = get_response(&v).unwrap();
    assert!(claimed.get_field_by_name("earnings").unwrap().as_list().unwrap().is_empty());
}

#[test]
fn fractional_isk_and_subsecond_timestamps() {
    let mut v = view();
    let mut isk = reward("isk"); isk.amount_isk = Some(crate::isk::IskAmount::parse_decimal("1.23").unwrap());
    v.rewards.push(isk);
    v.goal.period_start = infinity_core::consts::FILETIME_UNIX_EPOCH + 1;
    let get = roundtrip(&get_response(&v).unwrap());
    let a = get.get_field_by_name("goal").unwrap();
    let a = a.as_message().unwrap();
    let timestamp = a.get_field_by_name("active_after").unwrap();
    assert_eq!(timestamp.as_message().unwrap().get_field_by_name("nanos").unwrap().as_i32(), Some(100));
    let payment = a.get_field_by_name("payment").unwrap();
    let payment = payment.as_list().unwrap()[0].as_message().unwrap();
    let unit = payment.get_field_by_name("unit").unwrap();
    let isk = unit.as_message().unwrap().get_field_by_name("isk").unwrap();
    let amount = isk.as_message().unwrap().get_field_by_name("amount").unwrap();
    assert_eq!(amount.as_message().unwrap().get_field_by_name("units").unwrap().as_i64(), Some(1));
    assert_eq!(amount.as_message().unwrap().get_field_by_name("nanos").unwrap().as_i32(), Some(230_000_000));
}

#[test]
fn complete_goal_keeps_uuid_and_category_and_notice_uses_unit_only() {
    use crate::lua::content::daily_goals::model::{CompletionEntry, Matcher, Period};
    let mut v = view();
    v.snapshot.template.method = Method::CompleteGoal { entries: vec![CompletionEntry {
        goal: Matcher::Goal { uuid: "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".into() },
        category: Matcher::Month, period: Period::Day,
    }] };
    let mut sp = reward("sp"); sp.amount_units = Some(1); sp.omega_required = true; v.rewards.push(sp);
    let notice = named(&format!("{API}CurrentGoalsNotice")).unwrap();
    let g = goal(&notice, &v, true).unwrap();
    let earnings = g.get_field_by_name("earnings").unwrap();
    let earning = earnings.as_list().unwrap()[0].as_message().unwrap();
    assert!(earning.descriptor().get_field_by_name("omega_required").is_none());
    let attributes = g.get_field_by_name("goal").unwrap();
    let config = attributes.as_message().unwrap().get_field_by_name("contribution_configuration").unwrap();
    let branch = config.as_message().unwrap().get_field_by_name("complete_daily_goal").unwrap();
    let entries = branch.as_message().unwrap().get_field_by_name("contribution_methods").unwrap();
    let entry = entries.as_list().unwrap()[0].as_message().unwrap();
    let goal = entry.get_field_by_name("daily_goal").unwrap();
    assert!(goal.as_message().unwrap().has_field_by_name("goal"));
    let category = entry.get_field_by_name("daily_goal_category").unwrap();
    assert!(category.as_message().unwrap().has_field_by_name("category"));
    roundtrip(&g);
}

#[test]
fn isk_wire_limit_is_checked_without_rounding() {
    let mut v = view();
    let mut row = reward("isk");
    row.amount_isk = Some(crate::isk::IskAmount::parse_decimal("9223372036854775807.99").unwrap());
    v.rewards.push(row);
    roundtrip(&get_response(&v).unwrap());
    v.rewards[0].amount_isk = Some(crate::isk::IskAmount::parse_decimal("9223372036854775808.00").unwrap());
    assert!(get_response(&v).is_err());
}

#[test]
fn repeated_methods_and_explicit_any_are_preserved() {
    use crate::lua::content::daily_goals::model::{Matcher, SystemEntry};
    let mut v = view();
    v.snapshot.template.method = Method::KillNpc { entries: vec![SystemEntry { system: Matcher::Any }, SystemEntry { system: Matcher::Id { id: u64::MAX } }] };
    let m = roundtrip(&get_response(&v).unwrap());
    let goal = m.get_field_by_name("goal").unwrap();
    let configuration = goal.as_message().unwrap().get_field_by_name("contribution_configuration").unwrap();
    let kill = configuration.as_message().unwrap().get_field_by_name("kill_npc").unwrap();
    let methods = kill.as_message().unwrap().get_field_by_name("contribution_methods").unwrap();
    let entries = methods.as_list().unwrap();
    assert_eq!(entries.len(), 2);
    let first = entries[0].as_message().unwrap().get_field_by_name("solar_system").unwrap();
    assert!(first.as_message().unwrap().has_field_by_name("any"));
    assert_eq!(first.as_message().unwrap().get_field_by_name("any").unwrap().as_bool(), Some(true));
    let second = entries[1].as_message().unwrap().get_field_by_name("solar_system").unwrap();
    let exact = second.as_message().unwrap().get_field_by_name("match_value").unwrap();
    assert_eq!(exact.as_message().unwrap().get_field_by_name("sequential").unwrap().as_u64(), Some(u64::MAX));
}

#[test]
fn category_and_career_enums_match_descriptor_names() {
    for category in [Category::Ordinary, Category::Bonus, Category::Month] {
        for career in [Career::None, Career::Explorer, Career::Industrialist, Career::Enforcer, Career::SoldierOfFortune] {
            let mut v = view();
            v.snapshot.template.category = category;
            v.snapshot.template.career = career;
            let message = roundtrip(&get_response(&v).unwrap());
            let attributes = message.get_field_by_name("goal").unwrap();
            let attributes = attributes.as_message().unwrap();
            assert_eq!(*attributes.get_field_by_name("category").unwrap(), contribution::category(attributes, "category", category).unwrap());
            if career == Career::None { assert_eq!(*attributes.get_field_by_name("career").unwrap(), Value::EnumNumber(0)); }
        }
    }
}

#[test]
fn manufacturing_owner_and_location_oneofs_keep_presence() {
    use crate::lua::content::daily_goals::model::{ManufacturingEntry, Matcher, Owner};
    let mut v = view();
    v.snapshot.template.method = Method::ManufactureItem { entries: vec![
        ManufacturingEntry { owner: Owner::Any, facility: Matcher::Any, item: Matcher::Any },
        ManufacturingEntry { owner: Owner::Character, facility: Matcher::Station { id: 1 }, item: Matcher::Type { id: 1 } },
        ManufacturingEntry { owner: Owner::Corporation, facility: Matcher::Structure { id: 1 }, item: Matcher::Any },
    ] };
    let m = roundtrip(&get_response(&v).unwrap());
    let a = m.get_field_by_name("goal").unwrap();
    let config = a.as_message().unwrap().get_field_by_name("contribution_configuration").unwrap();
    let branch = config.as_message().unwrap().get_field_by_name("install_manufacturing_job").unwrap();
    let methods = branch.as_message().unwrap().get_field_by_name("contribution_methods").unwrap();
    for (entry, (owner, location)) in methods.as_list().unwrap().iter().zip([("any", "any"), ("match_any_character", "station"), ("match_any_corporation", "structure")]) {
        let entry = entry.as_message().unwrap();
        let o = entry.get_field_by_name("job_owner_type").unwrap();
        let l = entry.get_field_by_name("job_location").unwrap();
        assert!(o.as_message().unwrap().has_field_by_name(owner));
        assert!(l.as_message().unwrap().has_field_by_name(location));
    }
}

#[test]
fn identity_matchers_and_integer_boundaries() {
    use crate::lua::content::daily_goals::model::{CombatEntry, Matcher};
    for (identity, name) in [
        (Matcher::Any, "any"), (Matcher::Character { id: u32::MAX.into() }, "match_character_value"),
        (Matcher::Corporation { id: u32::MAX.into() }, "match_corporation_value"),
        (Matcher::Alliance { id: i32::MAX as u64 }, "match_alliance_value"),
        (Matcher::Faction { id: u64::MAX }, "match_faction_value"),
    ] {
        let mut v = view();
        v.snapshot.template.method = Method::DamageShip { entries: vec![CombatEntry { system: Matcher::Any, ship: Matcher::Any, identity }] };
        let m = roundtrip(&get_response(&v).unwrap());
        let a = m.get_field_by_name("goal").unwrap();
        let config = a.as_message().unwrap().get_field_by_name("contribution_configuration").unwrap();
        let branch = config.as_message().unwrap().get_field_by_name("damage_ship").unwrap();
        let methods = branch.as_message().unwrap().get_field_by_name("contribution_methods").unwrap();
        let identity = methods.as_list().unwrap()[0].as_message().unwrap().get_field_by_name("identity").unwrap();
        assert!(identity.as_message().unwrap().has_field_by_name(name));
    }
    let mut v = view();
    v.snapshot.template.method = Method::DamageShip { entries: vec![CombatEntry { system: Matcher::Any, ship: Matcher::Any, identity: Matcher::Character { id: u64::MAX } }] };
    assert!(get_response(&v).is_err());
}

#[test]
fn bonus_payment_order_and_stable_assets_follow_rows() {
    let mut v = view();
    v.snapshot.template.category = Category::Bonus;
    let mut first = reward("sp"); first.amount_units = Some(1);
    let mut second = reward("isk"); second.asset_id = "00000000-0000-0000-0000-000000000003".into(); second.amount_isk = Some(crate::isk::IskAmount::parse_decimal("1.00").unwrap());
    v.rewards = vec![first, second];
    let get = roundtrip(&get_response(&v).unwrap());
    let attributes = get.get_field_by_name("goal").unwrap();
    let payment = attributes.as_message().unwrap().get_field_by_name("payment").unwrap();
    let payment = payment.as_list().unwrap();
    let first = payment[0].as_message().unwrap();
    let second = payment[1].as_message().unwrap();
    assert!(first.get_field_by_name("unit").unwrap().as_message().unwrap().has_field_by_name("sp"));
    assert!(second.get_field_by_name("unit").unwrap().as_message().unwrap().has_field_by_name("isk"));
    let first_id = first.get_field_by_name("asset").unwrap();
    let second_id = second.get_field_by_name("asset").unwrap();
    assert_ne!(first_id, second_id);
    assert_eq!(get, roundtrip(&get_response(&v).unwrap()));
}

#[test]
fn every_config_supports_explicit_any() {
    use crate::lua::content::daily_goals::model::{CombatEntry, CompletionEntry, LoyaltyEntry, ManufacturingEntry, Matcher as M, MiningEntry, Owner, Period, ScanEntry, SystemEntry, WarfareEntry};
    let system = || SystemEntry { system: M::Any };
    let combat = || CombatEntry { system: M::Any, ship: M::Any, identity: M::Any };
    let warfare = || WarfareEntry { faction: M::Any, system: M::Any, archetype: M::Any };
    for method in [
        Method::KillNpc { entries: vec![system()] }, Method::DamageShip { entries: vec![combat()] },
        Method::MineOre { entries: vec![MiningEntry { system: M::Any, ore: M::Any }] },
        Method::CaptureFwComplex { entries: vec![warfare()] }, Method::DefendFwComplex { entries: vec![warfare()] },
        Method::RemoteRepairArmor { entries: vec![combat()] }, Method::RemoteRepairShields { entries: vec![combat()] },
        Method::ScanSignature { entries: vec![ScanEntry { system: M::Any, signature: M::Any }] },
        Method::ManufactureItem { entries: vec![ManufacturingEntry { owner: Owner::Any, facility: M::Any, item: M::Any }] },
        Method::CompleteGoal { entries: vec![CompletionEntry { goal: M::Any, category: M::Any, period: Period::Day }] },
        Method::SalvageWreck { entries: vec![system()] },
        Method::EarnLoyaltyPoints { entries: vec![LoyaltyEntry { corporation: M::Any }] },
        Method::SpaceJump,
    ] {
        let mut v = view(); v.snapshot.template.method = method;
        roundtrip(&get_response(&v).unwrap());
    }
}
