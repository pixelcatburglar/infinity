use super::model::{Definitions, Method, Reward, ValidationContext};

const DATA: &str = include_str!("../../../../lua/content/daily_goals/data/daily_goals.json");

#[test]
fn authored_dataset_has_distinct_eligible_pools_and_tracks() {
    let data: Definitions = serde_json::from_str(DATA).unwrap();
    assert_eq!(data.ordinary().len(), 10);
    assert_eq!(data.slot_pools().iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 2]);
    for pool in data.slot_pools() {
        for key in pool { assert!(data.ordinary().iter().any(|goal| &goal.key == key)); }
    }
    assert_eq!(data.bonus().target, 2);
    assert_eq!(data.monthly_base().iter().map(|g| g.target).collect::<Vec<_>>(), [3, 6, 9, 12]);
    assert_eq!(data.monthly_omega().iter().map(|g| g.target).collect::<Vec<_>>(), [3, 6, 9, 12]);
    assert!(data.ordinary().iter().all(|g| matches!(g.rewards.as_slice(), [Reward::Evermark { amount: 500 }])));
    assert!(data.ordinary().iter().any(|g| matches!(&g.method, Method::ManufactureItem { .. })));
}

#[test]
fn content_requires_real_method_metadata() {
    let sde = crate::sde::Sde::default();
    assert!(Definitions::parse(DATA.as_bytes(), "1", ValidationContext::Activation(&sde)).is_err());
    assert!(Definitions::parse(DATA.as_bytes(), "wrong-version", ValidationContext::Retained).is_err());
}

#[test]
fn retained_parser_keeps_structural_and_wire_validation() {
    use serde_json::json;
    let original: serde_json::Value = serde_json::from_str(DATA).unwrap();
    let parse = |v: &serde_json::Value| Definitions::parse(&serde_json::to_vec(v).unwrap(), "1", ValidationContext::Retained);
    assert!(parse(&original).is_ok());
    let mut invalid = original.clone();
    invalid["ordinary"][0]["method"]["kind"] = json!("not_a_method");
    assert!(parse(&invalid).is_err());
    let mut invalid = original.clone();
    invalid["ordinary"][1]["method"]["entries"][0]["identity"] = json!({"kind":"character","id":u32::MAX as u64 + 1});
    assert!(parse(&invalid).is_err());
    let mut invalid = original;
    invalid["ordinary"][0]["unknown"] = json!(true);
    assert!(parse(&invalid).is_err());
}

#[test]
fn reward_variants_are_typed_and_unknown_fields_refused() {
    let item: Reward = serde_json::from_str(r#"{"kind":"item","type_id":1,"amount":4}"#).unwrap();
    let plex: Reward = serde_json::from_str(r#"{"kind":"plex","whole_units":2}"#).unwrap();
    assert!(matches!(item, Reward::Item { type_id: 1, amount: 4 }));
    assert!(matches!(plex, Reward::Plex { whole_units: 2 }));
    assert!(serde_json::from_str::<Reward>(r#"{"kind":"plex","whole_units":2,"extra":1}"#).is_err());
    assert!(serde_json::from_str::<Reward>(r#"{"kind":"isk","amount":"1.001"}"#).is_err());
}

#[test]
fn every_contribution_branch_parses_explicit_matchers() {
    for json in [
        r#"{"kind":"kill_npc","entries":[{"system":{"kind":"any"}}]}"#,
        r#"{"kind":"damage_ship","entries":[{"system":{"kind":"any"},"ship":{"kind":"any"},"identity":{"kind":"any"}}]}"#,
        r#"{"kind":"mine_ore","entries":[{"system":{"kind":"any"},"ore":{"kind":"any"}}]}"#,
        r#"{"kind":"capture_fw_complex","entries":[{"system":{"kind":"any"},"faction":{"kind":"any"},"archetype":{"kind":"any"}}]}"#,
        r#"{"kind":"defend_fw_complex","entries":[{"system":{"kind":"any"},"faction":{"kind":"any"},"archetype":{"kind":"any"}}]}"#,
        r#"{"kind":"remote_repair_armor","entries":[{"system":{"kind":"any"},"ship":{"kind":"any"},"identity":{"kind":"any"}}]}"#,
        r#"{"kind":"remote_repair_shields","entries":[{"system":{"kind":"any"},"ship":{"kind":"any"},"identity":{"kind":"any"}}]}"#,
        r#"{"kind":"scan_signature","entries":[{"system":{"kind":"any"},"signature":{"kind":"any"}}]}"#,
        r#"{"kind":"manufacture_item","entries":[{"owner":"character","facility":{"kind":"any"},"item":{"kind":"any"}}]}"#,
        r#"{"kind":"complete_goal","entries":[{"goal":{"kind":"any"},"category":{"kind":"ordinary"},"period":"day"}]}"#,
        r#"{"kind":"salvage_wreck","entries":[{"system":{"kind":"any"}}]}"#,
        r#"{"kind":"earn_loyalty_points","entries":[{"corporation":{"kind":"any"}}]}"#,
        r#"{"kind":"space_jump"}"#,
    ] {
        assert!(serde_json::from_str::<Method>(json).is_ok(), "{json}");
    }
    assert!(serde_json::from_str::<Method>(r#"{"kind":"kill_npc","entries":[{}]}"#).is_err());
}

#[test]
fn snapshot_preserves_category_and_omega() {
    let mut template: super::model::Template = serde_json::from_str(r#"{"key":"snapshot","target":1,"career":"none","method":{"kind":"space_jump"},"rewards":[{"kind":"skill_points","amount":1}]}"#).unwrap();
    template.category = super::model::Category::Month;
    template.omega_required = true;
    let encoded = serde_json::to_string(&template).unwrap();
    let decoded: super::model::Template = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.category, super::model::Category::Month);
    assert!(decoded.omega_required);
}

#[test]
fn embedded_type_matchers_have_only_any_and_exact_type() {
    let pool = crate::gateway::message::pool();
    for (name, fields, oneof) in [
        ("eve_public.dailygoal.ShipTypeMatcher", ["any", "match_value"], "match_ship_type"),
        ("eve_public.dailygoal.ItemTypeMatcher", ["any", "item_type"], "match_item_type"),
        ("eve_public.dailygoal.OreTypeMatcher", ["any", "ore"], "match_ore_type"),
    ] {
        let message = pool.get_message_by_name(name).expect(name);
        let actual: Vec<_> = message.fields().map(|f| f.name().to_owned()).collect();
        assert_eq!(actual, fields);
        let oneofs: Vec<_> = message.oneofs().collect();
        assert_eq!(oneofs.len(), 1);
        assert_eq!(oneofs[0].name(), oneof);
        assert_eq!(oneofs[0].fields().map(|f| f.name().to_owned()).collect::<Vec<_>>(), fields);
    }
    let configuration = pool.get_message_by_name("eve_public.dailygoal.ContributionConfiguration").unwrap();
    let branches: Vec<_> = configuration.fields().map(|f| f.name().to_owned()).collect();
    assert_eq!(branches, ["kill_npc", "damage_ship", "mine_ore", "fw_capture", "fw_defend", "remote_armor_repair", "remote_shield_repair", "scan_signature", "install_manufacturing_job", "complete_daily_goal", "salvage_wreck", "earn_loyalty_points", "space_jump"]);
    let oneofs: Vec<_> = configuration.oneofs().collect();
    assert_eq!(oneofs.len(), 1);
    assert_eq!(oneofs[0].name(), "contribution_configuration");
    assert_eq!(oneofs[0].fields().count(), 13);
}

#[tokio::test]
#[ignore = "requires a dedicated database containing the native SDE"]
async fn actual_parse_validates_content_and_wire_bounds() {
    use serde_json::json;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.expect("native SDE is required");
    let parse = |v: &serde_json::Value| Definitions::parse(&serde_json::to_vec(v).unwrap(), "1", ValidationContext::Activation(&sde));
    let original: serde_json::Value = serde_json::from_str(DATA).unwrap();
    let defs = parse(&original).expect("production definitions require actual method metadata in both locales");
    assert_eq!(defs.ordinary().len(), 10);
    assert_eq!(defs.slot_pools().iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 2]);
    assert_eq!(defs.ordinary().iter().map(|t| t.target).collect::<Vec<_>>(), [25, 10000, 1000, 3, 100, 3, 1000, 5, 500, 500]);
    assert!(defs.ordinary().iter().all(|t| matches!(t.rewards.as_slice(), [Reward::Evermark { amount: 500 }])));
    assert_eq!(defs.bonus().target, 2);
    assert!(matches!(defs.bonus().rewards.first(), Some(Reward::SkillPoints { amount: 10000 })));
    assert_eq!(defs.monthly_base().iter().map(|t| t.target).collect::<Vec<_>>(), [3, 6, 9, 12]);
    assert_eq!(defs.monthly_omega().iter().map(|t| t.target).collect::<Vec<_>>(), [3, 6, 9, 12]);
    assert_eq!(defs.bonus().category, super::model::Category::Bonus);
    assert!(defs.monthly_omega().iter().all(|t| t.omega_required && t.category == super::model::Category::Month));
    let item = sde.types.iter().find(|t| t.type_id > 0).expect("SDE item type").type_id;
    for reward in [
        json!({"kind":"item","type_id":item,"amount":i32::MAX}),
        json!({"kind":"plex","whole_units":1}),
        json!({"kind":"isk","amount":"1.01"}),
        json!({"kind":"evermark","amount":1}),
        json!({"kind":"skill_points","amount":1}),
    ] {
        let mut v = original.clone(); v["ordinary"][0]["rewards"] = json!([reward]);
        parse(&v).expect("valid reward branch");
    }
    for reward in [
        json!({"kind":"item","type_id":item,"amount":i32::MAX as u64 + 1}),
        json!({"kind":"item","type_id":0,"amount":1}),
        json!({"kind":"plex","whole_units":u64::MAX}),
        json!({"kind":"evermark","amount":u64::MAX}),
        json!({"kind":"skill_points","amount":0}),
        json!({"kind":"isk","amount":"9223372036854775808"}),
        json!({"kind":"isk","amount":"0"}),
    ] {
        let mut v = original.clone(); v["ordinary"][0]["rewards"] = json!([reward]);
        assert!(parse(&v).is_err());
    }
    for (section, index, field, value) in [
        ("ordinary", 0, "category", json!("bonus")),
        ("ordinary", 0, "omega_required", json!(true)),
        ("monthly_base", 0, "omega_required", json!(true)),
        ("monthly_omega", 0, "omega_required", json!(false)),
        ("monthly_omega", 0, "category", json!("ordinary")),
    ] {
        let mut v = original.clone(); v[section][index][field] = value;
        assert!(parse(&v).is_err());
    }
    let mut v = original.clone(); v["bonus"]["category"] = json!("ordinary");
    assert!(parse(&v).is_err());
    let mut v = original.clone(); v["bonus"]["rewards"] = json!([{"kind":"plex","whole_units":1}]);
    assert!(parse(&v).is_err());
    let mut v = original.clone(); v["ordinary"][1]["method"]["entries"][0]["ship"] = json!({"kind":"group","id":1});
    assert!(parse(&v).is_err());
    let mut v = original.clone(); v["ordinary"][0]["unknown"] = json!(1);
    assert!(parse(&v).is_err());
    let mut v = original.clone(); v["bonus"]["category"] = json!("bonus"); v["monthly_omega"][0]["omega_required"] = json!(true);
    assert!(parse(&v).is_ok());
    let mut v = original.clone();
    v["bonus"]["target"] = json!(u64::MAX);
    v["bonus"]["rewards"] = json!([{"kind":"skill_points","amount":1}]);
    for (index, target) in [1_u64, 2, 3, u64::MAX].into_iter().enumerate() {
        v["monthly_base"][index]["target"] = json!(target);
        v["monthly_omega"][index]["target"] = json!(target);
    }
    assert!(parse(&v).is_ok());
    v["monthly_omega"][3]["target"] = json!(4);
    assert!(parse(&v).is_err());
}

#[tokio::test]
#[ignore = "requires a dedicated database containing the native SDE"]
async fn retained_package_reopens_after_publication_and_rejects_tampering() {
    use std::sync::Arc;
    use crate::lua::content::{self, ContentDefinitions, PinnedPolicy};
    use crate::lua::content::daily_goals::plans::{self, ProjectInput, Projection};
    use crate::lua::registry::Registry;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.expect("native SDE is required");
    let config = crate::config::lua::LuaConfig::default();
    let nonce = crate::handshake::random_u64();
    let a = format!("daily-retention-{nonce}-a");
    let b = format!("daily-retention-{nonce}-b");
    let item = sde.types.iter().find(|t| t.type_id > 0).expect("SDE item type").type_id;
    let build = |version: &str, source: &[u8]| {
        let mut data: serde_json::Value = serde_json::from_str(DATA).unwrap();
        data["version"] = version.into();
        data["ordinary"][0]["rewards"] = serde_json::json!([{"kind":"item","type_id":item,"amount":1}]);
        content::build_package(&mlua::Lua::new(), "daily_goals".into(), version.into(), b"registration".to_vec(), source.to_vec(), serde_json::to_vec(&data).unwrap(), &config, &sde, ValidationContext::Activation(&sde)).unwrap()
    };
    let old = Arc::new(build(&a, include_bytes!("../../../../lua/content/daily_goals/policy.lua")));
    let content::PackageDomain::DailyGoals(old_definitions) = &old.domain else { panic!("daily package expected") };
    let snapshot = crate::services::daily_goals::frozen::snapshot(&sde, &old_definitions.ordinary()[0]).unwrap();
    let frozen_json = serde_json::to_string(&snapshot).unwrap();
    let new = Arc::new(build(&b, b"return {Assign=function() end, Match=function() end, Cascade=function() end, Project=function() return {keys={}} end}"));
    let row_a = old.row();
    let row_b = new.row();
    assert!(crate::dao::lua_content::store(&db, &row_a).await.unwrap());
    assert!(crate::dao::lua_content::store(&db, &row_b).await.unwrap());
    for (row, time) in [(&row_a, 1), (&row_b, 2), (&row_a, 3)] {
        assert!(crate::dao::lua_content::activations::append_if_changed(&db, row, time).await.unwrap());
    }
    let history = crate::dao::lua_content::activations::list(&db, "daily_goals").await.unwrap();
    let own: Vec<_> = history.iter().filter(|r| r.version == a || r.version == b).map(|r| r.version.as_str()).collect();
    assert_eq!(own, [a.as_str(), b.as_str(), a.as_str()]);
    let mut content = ContentDefinitions::default();
    content.packages.insert("daily_goals".into(), new);
    let registry = Arc::new(Registry { lua: mlua::Lua::new(), claims: Default::default(), content, overrides: Default::default() });
    let current = PinnedPolicy::pin(registry.clone(), "daily_goals").unwrap();
    let retained_row = crate::dao::lua_content::load(&db, "daily_goals", &a).await.unwrap().unwrap();
    let post_update_sde = crate::sde::Sde::default();
    let retained = PinnedPolicy::reopen(registry.clone(), retained_row.clone(), &config, &post_update_sde).unwrap();
    assert_eq!(retained.daily_goals().unwrap().ordinary(), old_definitions.ordinary());
    let restored: crate::services::daily_goals::GoalSnapshot = serde_json::from_str(&frozen_json).unwrap();
    assert_eq!(restored.template, snapshot.template);
    assert_eq!((restored.messages.name, restored.messages.description, restored.messages.help, restored.assigner), (snapshot.messages.name, snapshot.messages.description, snapshot.messages.help, snapshot.assigner));
    assert!(retained.admit(&registry).is_err());
    assert!(current.admit(&registry).is_ok());
    let input = ProjectInput { entries: vec![Projection { key: "a".into(), selected: true }] };
    assert_eq!(plans::project(&retained, &input, None).unwrap().keys, ["a"]);
    assert!(plans::project(&current, &input, None).unwrap().keys.is_empty());
    for field in 0..4 {
        let mut damaged = retained_row.clone();
        match field {
            0 => damaged.source.push(b' '),
            1 => damaged.data.push(b' '),
            2 => damaged.version.push('x'),
            _ => damaged.fingerprint[0] ^= 1,
        }
        assert!(PinnedPolicy::reopen(registry.clone(), damaged, &config, &sde).is_err());
    }
}
