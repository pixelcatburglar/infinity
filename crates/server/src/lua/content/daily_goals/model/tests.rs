//! Boundaries for authored identifiers in daily contribution matchers.
use super::*;

fn valid(method: Method) -> bool { validate_method(&method, ValidationContext::Retained).is_ok() }

fn combat(identity: Matcher) -> Method {
    Method::DamageShip { entries: vec![CombatEntry { system: Matcher::Any, ship: Matcher::Any, identity }] }
}

#[test]
fn combat_identity_wire_bounds() {
    let constructors: [fn(u64) -> Matcher; 2] = [|id| Matcher::Character { id }, |id| Matcher::Corporation { id }];
    for make in constructors {
        for (id, expected) in [(0, false), (u32::MAX as u64, true), (u32::MAX as u64 + 1, false)] {
            assert_eq!(valid(combat(make(id))), expected);
        }
    }
    for (id, expected) in [(0, false), (i32::MAX as u64, true), (i32::MAX as u64 + 1, false)] {
        assert_eq!(valid(combat(Matcher::Alliance { id })), expected);
    }
    assert!(valid(combat(Matcher::Faction { id: u64::MAX })));
    assert!(!valid(combat(Matcher::Faction { id: 0 })));
    assert!(valid(combat(Matcher::Any)));
    assert!(!valid(Method::DamageShip { entries: vec![] }));
}

#[test]
fn generic_identifier_wire_bounds() {
    for (id, expected) in [(0, false), (u32::MAX as u64, true), (u32::MAX as u64 + 1, false)] {
        assert_eq!(valid(Method::ScanSignature { entries: vec![ScanEntry { system: Matcher::Any, signature: Matcher::Id { id } }] }), expected);
        assert_eq!(valid(Method::EarnLoyaltyPoints { entries: vec![LoyaltyEntry { corporation: Matcher::Id { id } }] }), expected);
        for capture in [true, false] {
            let entries = vec![WarfareEntry { faction: Matcher::Id { id: u64::MAX }, system: Matcher::Any, archetype: Matcher::Id { id } }];
            let method = if capture { Method::CaptureFwComplex { entries } } else { Method::DefendFwComplex { entries } };
            assert_eq!(valid(method), expected);
        }
    }
    assert!(valid(Method::ScanSignature { entries: vec![ScanEntry { system: Matcher::Any, signature: Matcher::Any }] }));
    assert!(valid(Method::EarnLoyaltyPoints { entries: vec![LoyaltyEntry { corporation: Matcher::Any }] }));
    assert!(valid(Method::CaptureFwComplex { entries: vec![WarfareEntry { faction: Matcher::Any, system: Matcher::Any, archetype: Matcher::Any }] }));
    assert!(!valid(Method::ScanSignature { entries: vec![] }));
    assert!(!valid(Method::EarnLoyaltyPoints { entries: vec![] }));
    assert!(!valid(Method::CaptureFwComplex { entries: vec![] }));
    assert!(!valid(Method::CaptureFwComplex { entries: vec![WarfareEntry { faction: Matcher::Id { id: 0 }, system: Matcher::Any, archetype: Matcher::Any }] }));
}

#[test]
fn localization_message_ref_wire_bounds() {
    for (id, expected) in [(-1, false), (0, false), (1, true), (u32::MAX as i64, true), (u32::MAX as i64 + 1, false)] {
        assert_eq!(positive_message_ref(id), expected);
    }
}

#[test]
fn committed_identifier_descriptor_kinds() {
    use prost_reflect::Kind;
    let pool = crate::gateway::message::pool();
    for (name, expected) in [
        ("eve_public.character.Identifier", Kind::Uint32),
        ("eve_public.corporation.Identifier", Kind::Uint32),
        ("eve_public.alliance.Identifier", Kind::Int32),
        ("eve_public.localization.message.Identifier", Kind::Uint32),
        ("eve_public.station.Identifier", Kind::Uint64),
        ("eve_public.structure.Identifier", Kind::Uint64),
    ] {
        let field = pool.get_message_by_name(name).unwrap().get_field_by_name("sequential").unwrap();
        assert_eq!(field.kind(), expected, "{name}.sequential");
    }
}

#[test]
fn completion_uuid_uses_shared_text_rules() {
    for (uuid, expected) in [
        ("00000000-0000-0000-0000-000000000000", true),
        ("ABCDEF01-2345-6789-abCD-EF0123456789", true),
        ("abcdef01-2345-6789-abcd-ef012345678", false),
        ("abcdef01_2345-6789-abcd-ef0123456789", false),
        ("abcdef01-2345-6789-abcd-ef012345678g", false),
    ] {
        let method = Method::CompleteGoal { entries: vec![CompletionEntry { goal: Matcher::Goal { uuid: uuid.into() }, category: Matcher::Any, period: Period::Day }] };
        assert_eq!(valid(method), expected, "{uuid}");
    }
}

#[test]
fn retained_warfare_definitions_reopen_without_producer_or_sde() {
    use serde_json::json;
    let original: serde_json::Value = serde_json::from_str(include_str!("../../../../../lua/content/daily_goals/data/daily_goals.json")).unwrap();
    for kind in ["capture_fw_complex", "defend_fw_complex"] {
        let mut value = original.clone();
        value["ordinary"][0]["method"] = json!({"kind":kind,"entries":[{"faction":{"kind":"any"},"system":{"kind":"any"},"archetype":{"kind":"any"}}]});
        assert!(Definitions::parse(&serde_json::to_vec(&value).unwrap(), "1", ValidationContext::Retained).is_ok());
    }
}

#[tokio::test]
#[ignore = "requires a dedicated database containing the native SDE"]
async fn activation_requires_adapter_capability_but_retained_content_reopens() {
    use serde_json::json;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Sde::load(&db).await.expect("native SDE is required");
    let original: serde_json::Value = serde_json::from_str(include_str!("../../../../../lua/content/daily_goals/data/daily_goals.json")).unwrap();
    let parse = |value: &serde_json::Value, context| Definitions::parse(&serde_json::to_vec(value).unwrap(), "1", context);
    assert!(parse(&original, ValidationContext::Activation(&sde)).is_ok());
    for kind in ["capture_fw_complex", "defend_fw_complex"] {
        let mut value = original.clone();
        value["ordinary"][0]["method"] = json!({"kind":kind,"entries":[{"faction":{"kind":"any"},"system":{"kind":"any"},"archetype":{"kind":"any"}}]});
        assert!(parse(&value, ValidationContext::Retained).is_ok());
        assert!(parse(&value, ValidationContext::Activation(&sde)).unwrap_err().to_string().contains("daily source unavailable"));
    }
    let mut value = original.clone();
    value["ordinary"][0]["method"] = value["bonus"]["method"].clone();
    assert!(parse(&value, ValidationContext::Activation(&sde)).unwrap_err().to_string().contains("daily source unavailable"));
    for section in ["bonus", "monthly_base", "monthly_omega"] {
        let mut value = original.clone();
        if section == "bonus" {
            value[section]["method"] = json!({"kind":"space_jump"});
        } else {
            value[section][0]["method"] = json!({"kind":"space_jump"});
        }
        assert!(parse(&value, ValidationContext::Retained).is_ok());
        assert!(parse(&value, ValidationContext::Activation(&sde)).unwrap_err().to_string().contains("daily source unavailable"));
    }
}
