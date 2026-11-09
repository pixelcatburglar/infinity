//! Pure policy behavior and registration rejection; no database is required.

use super::*;

fn package(source: &[u8]) -> Arc<Package> {
    let data = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/data/policy.json")).to_vec();
    Arc::new(Package {
        namespace: "sovereignty".into(), version: "1".into(), fingerprint: [0; 32],
        registration: Vec::new(), source: source.to_vec(),
        domain: PackageDomain::Sovereignty(policy::Values::parse(&data, "1").unwrap()), data,
        config: LuaConfig { call_budget_instructions: 10_000, instruction_hook_interval: 100, ..Default::default() },
    })
}

fn input() -> PolicyInput {
    PolicyInput { now: 100, system: 42, current_system: None, first_appointment: None, last_loss: None, not_before: None, owned_count: 1 }
}

fn evaluate(package: &Package, rule: Rule, input: &PolicyInput) -> mlua::Result<PolicyPlan> {
    policy::evaluate(&Lua::new(), package, rule, input)
}

#[test]
fn shipped_policy_handles_initialization_loss_and_destruction() {
    let source = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua"));
    let p = package(source);
    let i = input();
    let data: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/data/policy.json"))).unwrap();
    let cooldown = crate::client::filetime_ticks_from_seconds(data["capital_cooldown_seconds"].as_i64().unwrap());
    let destruction = crate::client::filetime_ticks_from_seconds(data["self_destruct_seconds"].as_i64().unwrap());
    let first = evaluate(&p, Rule::InitializeCapital, &i).unwrap();
    assert_eq!(first, PolicyPlan::Capital { current_system: 42, not_before: i.now + cooldown });
    assert_eq!(evaluate(&p, Rule::InitializeCapital, &i).unwrap(), first);
    for blocked in [
        PolicyInput { current_system: Some(42), ..i.clone() },
        PolicyInput { first_appointment: Some(1), ..i.clone() },
        PolicyInput { last_loss: Some(1), ..i.clone() },
        PolicyInput { owned_count: 2, ..i.clone() },
        PolicyInput { not_before: Some(i.now + 1), ..i.clone() },
    ] {
        assert_eq!(evaluate(&p, Rule::InitializeCapital, &blocked).unwrap(), PolicyPlan::Unchanged);
    }
    let lost_initial = PolicyInput { current_system: Some(42), ..i.clone() };
    assert_eq!(evaluate(&p, Rule::CapitalLost, &lost_initial).unwrap(), PolicyPlan::CapitalLost { not_before: i.now + cooldown });
    assert_eq!(evaluate(&p, Rule::CapitalLost, &PolicyInput { current_system: Some(43), ..i.clone() }).unwrap(), PolicyPlan::Unchanged);
    let lost = PolicyInput { current_system: Some(42), not_before: Some(i64::MAX), ..i.clone() };
    assert_eq!(evaluate(&p, Rule::CapitalLost, &lost).unwrap(), PolicyPlan::CapitalLost { not_before: i64::MAX });
    assert_eq!(evaluate(&p, Rule::CapitalLost, &i).unwrap(), PolicyPlan::Unchanged);
    assert_eq!(evaluate(&p, Rule::SelfDestruct, &i).unwrap(), PolicyPlan::SelfDestruct { due: i.now + destruction });
    assert!(evaluate(&p, Rule::InitializeCapital, &PolicyInput { now: i64::MIN, ..i.clone() }).is_ok());
    assert!(evaluate(&p, Rule::SelfDestruct, &PolicyInput { now: i64::MAX, ..i }).is_err());
}

#[test]
fn capabilities_input_mutation_and_shared_closure_state_are_unavailable() {
    for expression in ["os.time()", "math.random()", "host.x", "store.x", "require('x')"] {
        let source = format!("return {{ InitializeCapital = function(input) local value = {expression}; return {{kind='unchanged'}} end, CapitalLost=function() end, SelfDestruct=function() end }}");
        assert!(evaluate(&package(source.as_bytes()), Rule::InitializeCapital, &input()).is_err(), "{expression}");
    }
    let mutation = package(b"return {InitializeCapital=function(input) input.now=2; return {kind='unchanged'} end, CapitalLost=function() end, SelfDestruct=function() end}");
    let original = input();
    assert!(evaluate(&mutation, Rule::InitializeCapital, &original).is_err());
    assert_eq!(original.now, 100);
    let source = b"local n=0; return {InitializeCapital=function() n=n+1; if n~=1 then error('retained') end; return {kind='unchanged'} end, CapitalLost=function() end, SelfDestruct=function() end}";
    let p = package(source);
    let i = PolicyInput { owned_count: 0, ..input() };
    let lua = Lua::new();
    assert_eq!(policy::evaluate(&lua, &p, Rule::InitializeCapital, &i).unwrap(), PolicyPlan::Unchanged);
    assert_eq!(policy::evaluate(&lua, &p, Rule::InitializeCapital, &i).unwrap(), PolicyPlan::Unchanged);
}

#[test]
fn exhausted_budget_does_not_poison_runtime() {
    let bad = package(b"return {InitializeCapital=function() while true do end end, CapitalLost=function() end, SelfDestruct=function() end}");
    let good = package(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua")));
    let lua = Lua::new();
    assert!(policy::evaluate(&lua, &bad, Rule::InitializeCapital, &input()).is_err());
    assert!(policy::evaluate(&lua, &good, Rule::InitializeCapital, &input()).is_ok());
}

#[test]
fn initialization_and_rule_share_instruction_budget() {
    let make = |init: &str, body: &str| {
        let mut p = package(format!(
            "{init}; return {{InitializeCapital=function() {body}; return {{kind='unchanged'}} end, CapitalLost=function() end, SelfDestruct=function() end}}"
        ).as_bytes());
        Arc::get_mut(&mut p).unwrap().config.call_budget_ms = 60_000;
        p
    };
    let work = "for i=1,3000 do local x=i end";
    let lua = Lua::new();
    let i = input();
    assert!(policy::evaluate(&lua, &make(work, ""), Rule::InitializeCapital, &i).is_ok());
    assert!(policy::evaluate(&lua, &make("", work), Rule::InitializeCapital, &i).is_ok());
    let error = policy::evaluate(&lua, &make(work, work), Rule::InitializeCapital, &i).unwrap_err();
    assert!(error.to_string().contains("Lua execution budget exceeded"), "{error}");
    assert!(policy::evaluate(&lua, &make("", ""), Rule::InitializeCapital, &i).is_ok());
}

#[test]
fn duplicate_unknown_and_late_registrations_fail() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lua").canonicalize().unwrap();
    let source = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/sovereignty_content.lua"));
    let lua = Lua::new();
    let register = lua.create_table().unwrap();
    let env = lua.create_table().unwrap();
    env.set("register", register.clone()).unwrap();
    let definitions = Arc::new(Mutex::new(ContentDefinitions::default()));
    let open = Arc::new(AtomicBool::new(true));
    super::register(&lua, &register, definitions, open.clone(), &root, "sovereignty_content", source.to_vec(), &LuaConfig::default(), None, Arc::new(crate::sde::Sde::default())).unwrap();
    lua.load(source.as_slice()).set_environment(env.clone()).exec().unwrap();
    assert!(lua.load(source.as_slice()).set_environment(env.clone()).exec().is_err());
    assert!(lua.load("register.content{key='other',schema_version=1,content_version='1',datasets={},state_schema='sovereignty_policy_v1',operations={},progress_rules={}}").set_environment(env.clone()).exec().is_err());
    for bad in [
        "register.content{key='sovereignty',schema_version=2,content_version='1',datasets={},state_schema='sovereignty_policy_v1',operations={},progress_rules={}}",
        "register.content{key='sovereignty',schema_version=1,content_version='1',datasets={},state_schema='sovereignty_policy_v1',operations={'InitializeCapital','CapitalLost','Unknown'},progress_rules={}}",
        "register.content{key='sovereignty',schema_version=1,content_version='1',datasets={},state_schema='sovereignty_policy_v1',operations={},progress_rules={},extra=true}",
    ] {
        assert!(lua.load(bad).set_environment(env.clone()).exec().is_err());
    }
    open.store(false, Ordering::Release);
    assert!(lua.load(source.as_slice()).set_environment(env).exec().is_err());
}

#[test]
fn rejects_invalid_authored_durations() {
    for duration in ["0", "-1", "9223372036854775807", "1.5"] {
        let data = format!(r#"{{"version":"1","source":"test","capital_cooldown_seconds":{duration},"self_destruct_seconds":1,"capture_checkpoint_seconds":1}}"#);
        assert!(policy::Values::parse(data.as_bytes(), "1").is_err());
    }
}

#[test]
fn invalid_plans_and_missing_exports_are_rejected() {
    let invalid = package(b"return {InitializeCapital=function() return {kind='capital',current_system=42,not_before=1.0} end, CapitalLost=function() end, SelfDestruct=function() end}");
    assert!(evaluate(&invalid, Rule::InitializeCapital, &input()).is_err());
    let missing = package(b"return {InitializeCapital=function() end}");
    assert!(policy::validate(&Lua::new(), &missing).is_err());
    let overflow = PolicyInput { now: i64::MAX, ..input() };
    let shipped = package(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua")));
    assert!(evaluate(&shipped, Rule::InitializeCapital, &overflow).is_err());
}

#[test]
fn authored_lua_controls_legal_deadline() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua"));
    let original = "not_before = input.now + policy.capital_cooldown_ticks";
    assert!(source.contains(original));
    let modified = source.replacen(original, "not_before = input.now + 123", 1);
    let p = package(modified.as_bytes());
    assert_eq!(evaluate(&p, Rule::InitializeCapital, &input()).unwrap(),
        PolicyPlan::Capital { current_system: 42, not_before: 223 });
}

#[test]
fn plan_boundary_rejects_invalid_scope_variant_range_and_fields() {
    let script = |plan: &str| {
        let source = format!("return {{ InitializeCapital=function() return {plan} end, CapitalLost=function() return {plan} end, SelfDestruct=function() return {plan} end }}");
        package(source.as_bytes())
    };
    let i = input();
    for plan in [
        "{kind='capital',current_system=-42,not_before=101}",
        "{kind='capital',current_system=43,not_before=101}",
        "{kind='capital',current_system=42,not_before=100}",
        "{kind='capital',current_system=42,not_before=1.5}",
        "{kind='capital',current_system=42,not_before=101,extra=true}",
        "{kind='capital_lost',not_before=101}",
        "{kind='self_destruct',due=101}",
        "{kind='unknown'}",
    ] {
        assert!(evaluate(&script(plan), Rule::InitializeCapital, &i).is_err(), "{plan}");
    }
    let capital = script("{kind='capital',current_system=42,not_before=101}");
    assert!(evaluate(&capital, Rule::InitializeCapital, &PolicyInput { current_system: Some(42), ..i.clone() }).is_err());
    assert!(evaluate(&capital, Rule::InitializeCapital, &PolicyInput { not_before: Some(102), ..i.clone() }).is_err());
    assert!(evaluate(&capital, Rule::InitializeCapital, &PolicyInput { system: -1, ..i.clone() }).is_err());
    let lost = script("{kind='capital_lost',not_before=101}");
    assert!(evaluate(&lost, Rule::CapitalLost, &i).is_err());
    assert!(evaluate(&lost, Rule::CapitalLost, &PolicyInput { current_system: Some(43), ..i.clone() }).is_err());
    assert!(evaluate(&lost, Rule::CapitalLost, &PolicyInput { current_system: Some(42), not_before: Some(102), ..i.clone() }).is_err());
    assert!(evaluate(&lost, Rule::CapitalLost, &PolicyInput { current_system: Some(42), now: 101, ..i.clone() }).is_err());
    assert!(evaluate(&script("{kind='unchanged'}"), Rule::SelfDestruct, &i).is_err());
    assert!(evaluate(&script("{kind='self_destruct',due=100}"), Rule::SelfDestruct, &i).is_err());
    assert!(evaluate(&script("{kind='self_destruct',due=9223372036854775807}"),
        Rule::SelfDestruct, &PolicyInput { now: i64::MIN, ..i.clone() }).is_err());
    let shipped = package(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua")));
    assert!(evaluate(&shipped, Rule::CapitalLost, &PolicyInput { now: i64::MAX, current_system: Some(42), ..i }).is_err());
}

#[test]
fn content_handles_are_immutable_and_load_only() {
    let lua = Lua::new();
    let env = lua.create_table().unwrap();
    let open = Arc::new(AtomicBool::new(true));
    install_data(&lua, &env, None, open.clone()).unwrap();
    assert!(lua.load("return content.data('shipadaptionnodes') == nil").set_environment(env.clone()).eval::<bool>().unwrap());
    let bundle = Arc::new(bundle::Bundle { manifest: Vec::new(), files: Default::default(), parsed: Default::default(), chip_types: Vec::new() });
    install_data(&lua, &env, Some(bundle), open.clone()).unwrap();
    assert!(lua.load("local x = content.data('shipadaptionnodes'); x.name = 'other'").set_environment(env.clone()).exec().is_err());
    open.store(false, Ordering::Release);
    assert!(lua.load("return content.data('shipadaptionnodes')").set_environment(env).eval::<Value>().is_err());
}
