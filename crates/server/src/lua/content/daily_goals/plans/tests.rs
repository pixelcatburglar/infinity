use super::*;
use std::time::Duration;

const POLICY: &[u8] = include_bytes!("../../../../../lua/content/daily_goals/policy.lua");
fn execute(op: &str, input: Input) -> (Lua, Value) {
    let lua = Lua::new();
    let value = run(&lua, POLICY, &LuaConfig::default(), Some(op), Some(input), None).unwrap();
    (lua, value)
}
fn keys(op: &str, input: Input, allowed: &[&str]) -> Vec<String> {
    let (_lua, value) = execute(op, input);
    decode_keys(value, &allowed.iter().map(|s| s.to_string()).collect()).unwrap().keys
}
fn method(json: &str) -> Method { serde_json::from_str(json).unwrap() }
fn evaluate(m: &Method, fact: FactInput) -> Vec<String> {
    keys("Match", Input::Match { fact_method: fact.method.clone(), candidates: vec![MatchCandidate { key: "goal".into(), method: m.metadata_key(), entries: entries(m, &fact) }] }, &["goal"])
}
#[test]
fn assignment_is_deterministic_distinct_and_uses_remaining_pool() {
    let pools = vec![vec!["a".into(), "b".into()], vec!["c".into()], vec!["d".into()], vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()]];
    let input = Input::Assign { pools, entropy: [u64::MAX, 0, 0, u64::MAX] };
    let a = keys("Assign", input.clone(), &["a", "b", "c", "d", "e"]);
    assert_eq!(a, keys("Assign", input, &["a", "b", "c", "d", "e"]));
    assert_eq!(a.len(), 4); assert_eq!(a.iter().collect::<HashSet<_>>().len(), 4);
    assert_eq!(a[0], "b"); assert_eq!(a[1], "c"); assert_eq!(a[2], "d"); assert_eq!(a[3], "e");
}
#[test]
fn cascade_deduplicates_and_caps_and_project_filters() {
    let (_lua, value) = execute("Cascade", Input::Cascade { ordinary: vec!["a".into(), "a".into(), "b".into()], dates: vec!["x".into(), "x".into(), "y".into()], bonus: 1, month: 2 });
    let t = decode_table(value, &["bonus", "month"]).unwrap();
    assert!(matches!(t.raw_get::<Value>("bonus").unwrap(), Value::Integer(1)));
    assert!(matches!(t.raw_get::<Value>("month").unwrap(), Value::Integer(2)));
    assert_eq!(keys("Project", Input::Project(vec![Projection { key: "a".into(), selected: false }, Projection { key: "b".into(), selected: true }]), &["b"]), ["b"]);
}
#[test]
fn all_methods_run_typed_matchers_with_exact_ids() {
    let cases = [
        (r#"{"kind":"kill_npc","entries":[{"system":{"kind":"id","id":18446744073709551615}}]}"#, "KillNPC"),
        (r#"{"kind":"damage_ship","entries":[{"system":{"kind":"any"},"ship":{"kind":"type","id":5},"identity":{"kind":"character","id":18446744073709551615}}]}"#, "DamageShip"),
        (r#"{"kind":"mine_ore","entries":[{"system":{"kind":"any"},"ore":{"kind":"type","id":5}}]}"#, "MineOre"),
        (r#"{"kind":"capture_fw_complex","entries":[{"faction":{"kind":"id","id":9},"system":{"kind":"any"},"archetype":{"kind":"id","id":3}}]}"#, "CaptureFWComplex"),
        (r#"{"kind":"defend_fw_complex","entries":[{"faction":{"kind":"any"},"system":{"kind":"any"},"archetype":{"kind":"any"}}]}"#, "DefendFWComplex"),
        (r#"{"kind":"remote_repair_armor","entries":[{"system":{"kind":"any"},"ship":{"kind":"any"},"identity":{"kind":"any"}}]}"#, "RemoteRepairArmor"),
        (r#"{"kind":"remote_repair_shields","entries":[{"system":{"kind":"any"},"ship":{"kind":"any"},"identity":{"kind":"any"}}]}"#, "RemoteRepairShields"),
        (r#"{"kind":"scan_signature","entries":[{"system":{"kind":"any"},"signature":{"kind":"id","id":4}}]}"#, "ScanSignature"),
        (r#"{"kind":"manufacture_item","entries":[{"owner":"corporation","facility":{"kind":"station","id":8},"item":{"kind":"type","id":5}}]}"#, "ManufactureItem"),
        (r#"{"kind":"complete_goal","entries":[{"goal":{"kind":"goal","uuid":"00000000-0000-0000-0000-000000000001"},"category":{"kind":"bonus"},"period":"day"}]}"#, "CompleteGoal"),
        (r#"{"kind":"salvage_wreck","entries":[{"system":{"kind":"any"}}]}"#, "SalvageWreck"),
        (r#"{"kind":"earn_loyalty_points","entries":[{"corporation":{"kind":"id","id":6}}]}"#, "EarnLoyaltyPoints"),
        (r#"{"kind":"space_jump"}"#, "SpaceJump"),
    ];
    for (json, name) in cases {
        let m = method(json); let fact = FactInput { method: name.into(), system: Some(u64::MAX), type_id: Some(5), character: Some(u64::MAX), corporation: Some(6), faction: Some(9), facility: Some(8), facility_is_station: Some(true), owner: Some(Owner::Corporation), signature: Some(4), archetype: Some(3), goal: Some("00000000-0000-0000-0000-000000000001".into()), category: Some(Category::Bonus), period: Some(Period::Day), ..Default::default() };
        assert_eq!(evaluate(&m, fact.clone()), ["goal"], "{name}");
        assert!(evaluate(&m, FactInput { method: "wrong".into(), ..fact }).is_empty());
    }
}

#[test]
fn completion_matches_mixed_case_uuid() {
    let m = method(r#"{"kind":"complete_goal","entries":[{"goal":{"kind":"goal","uuid":"ABCDef01-2345-6789-abCD-ef0123456789"},"category":{"kind":"ordinary"},"period":"day"}]}"#);
    let fact = FactInput { method: "CompleteGoal".into(), goal: Some("abcdef01-2345-6789-abcd-ef0123456789".into()), category: Some(Category::Ordinary), period: Some(Period::Day), ..Default::default() };
    assert_eq!(evaluate(&m, fact), ["goal"]);
}
#[test]
fn manufacturing_requires_exact_facility_kind_and_id() {
    let station = method(r#"{"kind":"manufacture_item","entries":[{"owner":"character","facility":{"kind":"station","id":18446744073709551615},"item":{"kind":"any"}}]}"#);
    let structure = method(r#"{"kind":"manufacture_item","entries":[{"owner":"character","facility":{"kind":"structure","id":18446744073709551615},"item":{"kind":"any"}}]}"#);
    let mut fact = FactInput { method: "ManufactureItem".into(), facility: Some(u64::MAX), facility_is_station: Some(true), owner: Some(Owner::Character), ..Default::default() };
    assert_eq!(evaluate(&station, fact.clone()), ["goal"]);
    assert!(evaluate(&structure, fact.clone()).is_empty());
    fact.facility_is_station = Some(false);
    assert_eq!(evaluate(&structure, fact.clone()), ["goal"]);
    assert!(evaluate(&station, fact.clone()).is_empty());
    fact.facility_is_station = None;
    assert!(evaluate(&station, fact.clone()).is_empty());
    assert!(evaluate(&structure, fact.clone()).is_empty());
    fact.facility_is_station = Some(true);
    fact.facility = Some(u64::MAX - 1);
    assert!(evaluate(&station, fact.clone()).is_empty());
    fact.facility_is_station = Some(false);
    assert!(evaluate(&structure, fact.clone()).is_empty());
    fact.facility = None;
    assert!(evaluate(&structure, fact).is_empty());
}
#[test]
fn manufacturing_any_owner_and_facility_are_wildcards() {
    let m = method(r#"{"kind":"manufacture_item","entries":[{"owner":"any","facility":{"kind":"any"},"item":{"kind":"any"}}]}"#);
    let mut fact = FactInput { method: "ManufactureItem".into(), ..Default::default() };
    assert_eq!(evaluate(&m, fact.clone()), ["goal"]);
    fact.facility = Some(u64::MAX);
    fact.facility_is_station = Some(false);
    fact.owner = Some(Owner::Corporation);
    assert_eq!(evaluate(&m, fact), ["goal"]);
}
#[test]
fn conditions_are_and_within_entry_or_between_entries() {
    let m = method(r#"{"kind":"complete_goal","entries":[{"goal":{"kind":"goal","uuid":"00000000-0000-0000-0000-000000000001"},"category":{"kind":"bonus"},"period":"day"},{"goal":{"kind":"any"},"category":{"kind":"ordinary"},"period":"month"}]}"#);
    let mut f = FactInput { method: "CompleteGoal".into(), goal: Some("00000000-0000-0000-0000-000000000001".into()), category: Some(Category::Bonus), period: Some(Period::Month), ..Default::default() };
    assert!(evaluate(&m, f.clone()).is_empty()); f.category = Some(Category::Ordinary); assert_eq!(evaluate(&m, f), ["goal"]);
}
#[test]
fn decoder_rejects_duplicate_unknown_sparse_and_extra_fields() {
    let lua = Lua::new(); let allowed = ["a".to_owned()].into_iter().collect();
    for source in ["return {keys={'a','a'}}", "return {keys={'b'}}", "return {keys={[2]='a'}}", "return {keys={'a'}, extra=true}", "return {keys={1}}"] {
        assert!(decode_keys(lua.load(source).eval().unwrap(), &allowed).is_err(), "{source}");
    }
}
#[test]
fn closed_runner_rejects_capabilities_exports_and_deadlines() {
    let lua = Lua::new(); let cfg = LuaConfig::default();
    validate(&lua, POLICY, &cfg).unwrap();
    assert!(validate(&lua, b"return {Assign=function() end}", &cfg).is_err());
    assert!(run(&lua, b"return os.execute('x')", &cfg, None, None, None).is_err());
    assert!(run(&lua, POLICY, &cfg, None, None, Some(Instant::now() - Duration::from_secs(1))).is_err());
    let cfg = LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..cfg };
    assert!(run(&lua, b"while true do end", &cfg, None, None, None).is_err());
}
#[test]
fn captured_source_not_current_source_decides_projection() {
    let old = b"return {Assign=function() end, Match=function() end, Cascade=function() end, Project=function() return {keys={'a'}} end}";
    let new = b"return {Assign=function() end, Match=function() end, Cascade=function() end, Project=function() return {keys={}} end}";
    let lua = Lua::new(); let cfg = LuaConfig::default(); let allowed = ["a".to_owned()].into_iter().collect();
    let input = Input::Project(vec![Projection { key: "a".into(), selected: true }]);
    assert_eq!(decode_keys(run(&lua, old, &cfg, Some("Project"), Some(input.clone()), None).unwrap(), &allowed).unwrap().keys, ["a"]);
    assert!(decode_keys(run(&lua, new, &cfg, Some("Project"), Some(input), None).unwrap(), &allowed).unwrap().keys.is_empty());
}

#[test]
fn cascade_accepts_full_u64_targets_through_pinned_policy() {
    use std::sync::Arc;
    use crate::lua::content::{ContentDefinitions, Package, PackageDomain, PinnedPolicy};
    use crate::lua::registry::Registry;
    let original: serde_json::Value = serde_json::from_str(include_str!("../../../../../lua/content/daily_goals/data/daily_goals.json")).unwrap();
    for target in [i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
        let mut data = original.clone();
        data["bonus"]["target"] = target.into();
        data["monthly_base"][3]["target"] = target.into();
        data["monthly_omega"][3]["target"] = target.into();
        let definitions = serde_json::from_value(data).unwrap();
        let package = Package {
            namespace: "daily_goals".into(), version: "1".into(), fingerprint: [0; 32],
            registration: vec![], source: POLICY.to_vec(), data: vec![], config: LuaConfig::default(),
            domain: PackageDomain::DailyGoals(Arc::new(definitions)),
        };
        let mut content = ContentDefinitions::default();
        content.packages.insert("daily_goals".into(), Arc::new(package));
        let registry = Arc::new(Registry { lua: Lua::new(), claims: Default::default(), content, overrides: Default::default() });
        let pin = PinnedPolicy::pin(registry, "daily_goals").unwrap();
        let input = CascadeInput { ordinary_ids: vec!["a".into(), "a".into(), "b".into()], bonus_dates: vec!["x".into(), "x".into(), "y".into()] };
        assert_eq!(cascade(&pin, &input, None).unwrap(), CascadePlan { bonus: 2, month: 2 });
    }
}
