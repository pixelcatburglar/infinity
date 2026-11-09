use super::*;
use infinity_core::consts::GROUP_ADAPTION_CHIPS;

fn texts() -> BTreeMap<String, String> {
    REQUIRED_MESSAGES.iter().copied().chain(["refused"]).map(|key| (key.to_owned(), "text".to_owned())).collect()
}

#[test]
fn quotes_use_corporation_override_and_fallback() {
    let definitions = Definitions {
        chip_types: vec![1, 2], trees: [1].into_iter().collect(),
        quotes: [(1, Quote { default: 5, overrides: [(2, 7)].into_iter().collect() })].into_iter().collect(),
        issuers: [2].into_iter().collect(), messages: [("refused".into(), "text".into())].into_iter().collect(),
    };
    assert_eq!(definitions.quote(1, 2), Some(7));
    assert_eq!(definitions.quote(1, 3), Some(5));
    assert_eq!(definitions.quote(2, 2), None);
    assert_eq!(definitions.message("refused"), Some("text"));
    assert!(definitions.published_tree(1));
    assert!(definitions.issuer(2));
}

#[test]
fn injector_attributes_require_positive_integral_supported_values() {
    use infinity_core::consts::dogma::{ATTR_REFIT_TICKET_QUANTITY as QUANTITY, ATTR_REFIT_TICKET_TYPE as TARGET};
    use crate::services::ship_adaption::policy::conversion;
    let definitions = Definitions { chip_types: vec![1], trees: BTreeSet::new(), quotes: BTreeMap::new(), issuers: BTreeSet::new(), messages: texts() };
    let make = |attrs| crate::sde::types::tests::attribute_fixture(&[(1, None), (2, Some(GROUP_ADAPTION_CHIPS))], &[(2, attrs)]);
    assert_eq!(conversion(&make(vec![(TARGET, 1.0), (QUANTITY, 10.0)]), &definitions, 2, 3), Some((1, 30)));
    for value in [0.0, -1.0, 1.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::from(u32::MAX) + 1.0] {
        assert_eq!(conversion(&make(vec![(TARGET, 1.0), (QUANTITY, value)]), &definitions, 2, 1), None);
        assert_eq!(conversion(&make(vec![(TARGET, value), (QUANTITY, 1.0)]), &definitions, 2, 1), None);
    }
    for attrs in [vec![], vec![(TARGET, 1.0)], vec![(QUANTITY, 1.0)], vec![(TARGET, 3.0), (QUANTITY, 1.0)]] {
        assert_eq!(conversion(&make(attrs), &definitions, 2, 1), None);
    }
    assert_eq!(conversion(&make(vec![(TARGET, 1.0), (QUANTITY, 10.0)]), &definitions, 2, u32::MAX), None);
    assert_eq!(conversion(&make(vec![(TARGET, 1.0), (QUANTITY, 1.0)]), &definitions, 1, 1), None);
}

#[test]
fn reload_cannot_drop_chip_type() {
    let make = |chip_types| Definitions { chip_types, trees: BTreeSet::new(), quotes: BTreeMap::new(), issuers: BTreeSet::new(), messages: BTreeMap::new() };
    assert!(make(vec![1, 2]).compatible(&make(vec![1])));
    assert!(!make(vec![2]).compatible(&make(vec![1, 2])));
}

fn fixture() -> Bundle {
    use serde_json::json;
    let tier = json!({"consume_multiplier":[1.0], "base_unlock_items":[{"amount":1,"item_type_id":1}]});
    let rows = [
        ("shipadaptionnodes", "1", json!({"_key":1,"node_type":0,"unlock_type":0})),
        ("shipadaptiontrees", "1", json!({"_key":1,"tree_nodes":[{"id":1,"node_id":1,"parent_id":-1}],"t1_node_consume":tier,"t2_node_consume":tier,"t3_node_consume":tier})),
        ("shipadaptionnodetypes", "0", json!({"_key":0,"name":"Root"})),
        ("shipadaptionnodeunlocktypes", "0", json!({"_key":0,"name":"test"})),
        ("shipadaptionconfig", "", json!({"factions":[{"faction_id":1,"ship_groups":[{"group_id":1,"tree_id":1}]}]})),
        ("shipadaptionchips", "1", json!({"_key":1,"defaultCorpBonus":{"lpBonus":2},"corpsBonus":{}})),
        ("shipadaptionchipexchangeconfig", "", json!({"availableCorpIDs":[]})),
    ];
    let mut files = BTreeMap::new();
    let mut datasets = BTreeMap::new();
    for (name, key, data) in rows {
        let bytes = serde_json::to_vec(&json!({"schema":"cfsd-adaptation-dataset","schema_version":1,"client_build":"test","source_resource":"test","records":[{"key":key,"data":data}]})).unwrap();
        datasets.insert(name, json!({"path":format!("datasets/{name}.json"),"records":1,"sha256":crate::hash::sha256_hex(&bytes)}));
        files.insert(name.to_owned(), bytes);
    }
    let manifest = serde_json::to_vec(&json!({"schema":"infinity-adaption-content","schema_version":1,"client_build":"test","source_manifest_sha256":"0".repeat(64),"constants_sha256":"0".repeat(64),"chip_types":[1],"datasets":datasets,"provenance":{"source_validation":{},"chip_type_symbols":["test"]},"validation":{"scope":"chip-ledger","node_task_effect_gameplay_enabled":false}})).unwrap();
    let mut bundle = Bundle { manifest, files, parsed: BTreeMap::new(), chip_types: Vec::new() };
    bundle.validate(1_000_000).unwrap();
    bundle
}

fn refresh(bundle: &mut Bundle) {
    let mut manifest: Value = serde_json::from_slice(&bundle.manifest).unwrap();
    for (name, bytes) in &bundle.files {
        manifest["datasets"][name.as_str()]["sha256"] = serde_json::json!(crate::hash::sha256_hex(bytes));
    }
    bundle.manifest = serde_json::to_vec(&manifest).unwrap();
    bundle.validate(1_000_000).unwrap();
}

#[test]
fn catalogue_rebuild_and_reference_guards() {
    let bundle = fixture();
    let sde = crate::sde::types::tests::placement_fixture(&[(1, None)]);
    let values = build(&bundle, &sde, texts()).unwrap();
    assert_eq!(values.chip_types(), &[1]);
    assert!(values.published_tree(1));
    assert_eq!(values.quote(1, 1), Some(2));
    let mut invalid = bundle.clone();
    let mut manifest: Value = serde_json::from_slice(&invalid.manifest).unwrap();
    manifest["chip_types"] = serde_json::json!([1, 999]);
    invalid.manifest = serde_json::to_vec(&manifest).unwrap();
    refresh(&mut invalid);
    assert!(build(&invalid, &sde, texts()).is_err());
}

#[test]
fn catalogue_rejects_unknown_references_and_overflow() {
    let sde = crate::sde::types::tests::placement_fixture(&[(1, Some(GROUP_ADAPTION_CHIPS))]);
    for (family, field, replacement) in [
        ("shipadaptionnodes", "node_type", serde_json::json!(9)),
        ("shipadaptionnodes", "unlock_skill_id", serde_json::json!(1)),
        ("shipadaptionnodes", "target_ship_types", serde_json::json!([1])),
        ("shipadaptionchips", "unexpected", serde_json::json!(1)),
    ] {
        let mut bundle = fixture();
        let mut data: Value = serde_json::from_slice(&bundle.files[family]).unwrap();
        data["records"][0]["data"][field] = replacement;
        bundle.files.insert(family.to_owned(), serde_json::to_vec(&data).unwrap());
        refresh(&mut bundle);
        assert!(build(&bundle, &sde, texts()).is_err());
    }
    let mut bundle = fixture();
    let family = "shipadaptiontrees";
    let mut data: Value = serde_json::from_slice(&bundle.files[family]).unwrap();
    data["records"][0]["data"]["t1_node_consume"]["consume_multiplier"] = serde_json::json!([1e20]);
    bundle.files.insert(family.to_owned(), serde_json::to_vec(&data).unwrap());
    refresh(&mut bundle);
    assert!(build(&bundle, &sde, texts()).is_err());
}

#[test]
fn empty_multiplier_is_allowed_only_when_tier_is_unreachable() {
    let sde = crate::sde::types::tests::placement_fixture(&[(1, Some(GROUP_ADAPTION_CHIPS))]);
    let mut bundle = fixture();
    let trees = "shipadaptiontrees";
    let mut data: Value = serde_json::from_slice(&bundle.files[trees]).unwrap();
    data["records"][0]["data"]["t1_node_consume"]["consume_multiplier"] = serde_json::json!([]);
    bundle.files.insert(trees.to_owned(), serde_json::to_vec(&data).unwrap());
    refresh(&mut bundle);
    assert!(build(&bundle, &sde, texts()).is_ok());
    let types = "shipadaptionnodetypes";
    let mut data: Value = serde_json::from_slice(&bundle.files[types]).unwrap();
    data["records"][0]["data"]["name"] = serde_json::json!("T1");
    bundle.files.insert(types.to_owned(), serde_json::to_vec(&data).unwrap());
    refresh(&mut bundle);
    assert!(build(&bundle, &sde, texts()).is_err());
}

#[test]
fn dangling_layout_parent_is_rejected() {
    let sde = crate::sde::types::tests::placement_fixture(&[(1, Some(GROUP_ADAPTION_CHIPS))]);
    let mut bundle = fixture();
    let family = "shipadaptiontrees";
    let mut data: Value = serde_json::from_slice(&bundle.files[family]).unwrap();
    data["records"][0]["data"]["tree_nodes"][0]["parent_id"] = serde_json::json!(2);
    bundle.files.insert(family.to_owned(), serde_json::to_vec(&data).unwrap());
    refresh(&mut bundle);
    assert!(build(&bundle, &sde, texts()).is_err());
}

#[test]
fn unlock_chip_must_be_supported_but_not_necessarily_quoted() {
    let sde = crate::sde::types::tests::placement_fixture(&[(1, Some(GROUP_ADAPTION_CHIPS)), (2, Some(GROUP_ADAPTION_CHIPS))]);
    let mut bundle = fixture();
    let trees = "shipadaptiontrees";
    let mut tree: Value = serde_json::from_slice(&bundle.files[trees]).unwrap();
    tree["records"][0]["data"]["t1_node_consume"]["base_unlock_chips"] = serde_json::json!([{"amount":1,"item_type_id":2}]);
    bundle.files.insert(trees.to_owned(), serde_json::to_vec(&tree).unwrap());
    refresh(&mut bundle);
    assert!(build(&bundle, &sde, texts()).is_err());
    let mut manifest: Value = serde_json::from_slice(&bundle.manifest).unwrap();
    manifest["chip_types"] = serde_json::json!([1, 2]);
    bundle.manifest = serde_json::to_vec(&manifest).unwrap();
    refresh(&mut bundle);
    let definitions = build(&bundle, &sde, texts()).unwrap();
    assert_eq!(definitions.quote(2, 1), None);
}

#[test]
fn retained_catalogue_reopens_without_bundle_directory() {
    let bundle = fixture();
    let sde = crate::sde::types::tests::placement_fixture(&[(1, Some(GROUP_ADAPTION_CHIPS))]);
    let config = crate::config::lua::LuaConfig::default();
    let lua = mlua::Lua::new();
    let catalogue = build(&bundle, &sde, texts()).unwrap();
    let version = format!("test/{}", bundle.manifest_hash());
    let envelope = super::super::AdaptionData {
        bundle, messages_source: b"return {refused='text', invalid='text', conflict='text', item='text', chips='text', overflow='text'}".to_vec(),
        messages: texts(), catalogue,
    };
    let data = serde_json::to_vec(&envelope).unwrap();
    let source = b"return {InjectChips=function() end, ChipExchange=function() end}".to_vec();
    let package = super::super::build_package(&lua, "ship_adaption".into(), version, b"registration".to_vec(), source, data, &config, &sde, super::super::daily_goals::model::ValidationContext::Retained).unwrap();
    let row = package.row();
    let reopened = super::super::decode_retained_adaption(&row, &config, &sde).unwrap();
    assert_eq!(reopened.message("refused"), Some("text"));
    assert_eq!(reopened.quote(1, 1), Some(2));
    let package = std::sync::Arc::new(package);
    let make_registry = || {
        let mut content = super::super::ContentDefinitions::default();
        content.packages.insert("ship_adaption".into(), package.clone());
        std::sync::Arc::new(crate::lua::registry::Registry {
            lua: mlua::Lua::new(), claims: Default::default(), content, overrides: Default::default(),
        })
    };
    let old = make_registry();
    let pinned = super::super::PinnedPolicy::pin(old.clone(), "ship_adaption").unwrap();
    let admitted = pinned.admit(&old).unwrap();
    assert!(std::sync::Arc::ptr_eq(pinned.adaption().unwrap(), admitted.policy().adaption().unwrap()));
    let next = make_registry();
    assert!(pinned.admit(&next).is_err());
    assert_eq!(admitted.policy().adaption().unwrap().message("refused"), Some("text"));
}

#[test]
fn repository_policy_plans_exact_quantities_quotes_and_checked_totals() {
    use crate::services::ship_adaption::policy::{self, Input};
    let definitions = Definitions {
        chip_types: vec![1, 2], trees: BTreeSet::new(), issuers: [3, 4].into_iter().collect(),
        quotes: [(1, Quote { default: 5, overrides: [(3, 7)].into_iter().collect() })].into_iter().collect(),
        messages: texts(),
    };
    let make_pin = |definitions: Definitions| {
        let package = super::super::Package {
            namespace: "ship_adaption".into(), version: "test".into(), fingerprint: [0; 32], registration: vec![],
            source: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/ship_adaption/policy.lua")).to_vec(),
            data: vec![], config: crate::config::lua::LuaConfig::default(),
            domain: super::super::PackageDomain::Adaption { definitions: std::sync::Arc::new(definitions), bundle: std::sync::Arc::new(fixture()) },
        };
        let mut content = super::super::ContentDefinitions::default();
        content.packages.insert("ship_adaption".into(), std::sync::Arc::new(package));
        let registry = std::sync::Arc::new(crate::lua::registry::Registry { lua: mlua::Lua::new(), claims: Default::default(), content, overrides: Default::default() });
        let pin = super::super::PinnedPolicy::pin(registry.clone(), "ship_adaption").unwrap();
        (pin, registry)
    };
    let (pin, registry) = make_pin(definitions.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let input = |pin: &super::super::PinnedPolicy, issuer, chips| Input { definitions: pin.adaption().unwrap().clone(), item_type: None, quantity: 0, credit: None, issuer, chips: std::sync::Arc::new(chips) };
    let inject = Input { definitions: pin.adaption().unwrap().clone(), item_type: Some(3), quantity: 9, credit: Some((1, 90)), issuer: 0, chips: std::sync::Arc::new(vec![]) };
    assert!(policy::plan(&pin, inject, deadline).unwrap().unwrap().admit(std::sync::Arc::new(pin.admit(&registry).unwrap())).unwrap().is_some());
    for issuer in [3, 4] {
        assert!(policy::plan(&pin, input(&pin, issuer, vec![(1, 2)]), deadline).unwrap().unwrap().admit(std::sync::Arc::new(pin.admit(&registry).unwrap())).unwrap().is_some());
    }
    assert!(policy::plan(&pin, input(&pin, 3, vec![(2, 1)]), deadline).unwrap().is_none());
    assert!(policy::plan(&pin, input(&pin, 9, vec![(1, 1)]), deadline).unwrap().is_none());
    let mut zero = definitions.clone();
    zero.quotes.get_mut(&1).unwrap().default = 0;
    let (zero_pin, _) = make_pin(zero);
    assert!(policy::plan(&zero_pin, input(&zero_pin, 4, vec![(1, 1)]), deadline).unwrap().is_none());
    let mut large = definitions.clone();
    large.quotes.get_mut(&1).unwrap().default = i64::MAX;
    let (large_pin, large_registry) = make_pin(large);
    let prepared = policy::plan(&large_pin, input(&large_pin, 4, vec![(1, 2)]), deadline).unwrap().unwrap();
    assert!(matches!(prepared.admit(std::sync::Arc::new(large_pin.admit(&large_registry).unwrap())), Err(crate::services::CallError::User { .. })));
}

#[test]
fn malformed_authored_plans_are_closed_and_integer_only() {
    use std::{sync::Arc, time::{Duration, Instant}};
    use crate::services::ship_adaption::policy::{self, Input};
    let definitions = Arc::new(Definitions {
        chip_types: vec![1, 2], trees: BTreeSet::new(), issuers: [3].into_iter().collect(),
        quotes: [(1, Quote { default: 5, overrides: BTreeMap::new() }), (2, Quote { default: 5, overrides: BTreeMap::new() })].into_iter().collect(), messages: texts(),
    });
    for (exchange, result) in [
        (false, "{type_id=1.0, quantity=1}"),
        (false, "{type_id='1', quantity=1}"),
        (false, "{type_id=true, quantity=1}"),
        (false, "{type_id=1, quantity=0}"),
        (false, "{type_id=1, quantity=-1}"),
        (false, "{type_id=1, quantity=4294967296}"),
        (false, "{type_id=2147483648, quantity=1}"),
        (false, "{type_id=1}"),
        (false, "{type_id=1, quantity=1, extra=true}"),
        (true, "{{type_id=1,quantity=1,quote=5.0}}"),
        (true, "{{type_id=1,quantity=1,quote='5'}}"),
        (true, "{{type_id=1,quantity=true,quote=5}}"),
        (true, "{{type_id=1,quantity=1,quote=-1}}"),
        (true, "{{type_id=1,quantity=1,quote=6}}"),
        (true, "{{type_id=1,quantity=1,quote=5,extra=1}}"),
        (true, "{[1]={type_id=1,quantity=1,quote=5},extra=true}"),
        (true, "{[2]={type_id=1,quantity=1,quote=5}}"),
        (true, "{{type_id=2,quantity=1,quote=5},{type_id=1,quantity=1,quote=5}}"),
    ] {
        let source = format!("return {{InjectChips=function() return {result} end, ChipExchange=function() return {result} end}}");
        let package = super::super::Package {
            namespace: "ship_adaption".into(), version: "test".into(), fingerprint: [0; 32], registration: vec![],
            source: source.into_bytes(), data: vec![], config: crate::config::lua::LuaConfig::default(),
            domain: super::super::PackageDomain::Adaption { definitions: definitions.clone(), bundle: Arc::new(fixture()) },
        };
        let mut content = super::super::ContentDefinitions::default();
        content.packages.insert("ship_adaption".into(), Arc::new(package));
        let registry = Arc::new(crate::lua::registry::Registry { lua: mlua::Lua::new(), claims: Default::default(), content, overrides: Default::default() });
        let pin = super::super::PinnedPolicy::pin(registry, "ship_adaption").unwrap();
        let chips = if result.contains("type_id=2,") { vec![(1, 1), (2, 1)] } else { vec![(1, 1)] };
        let input = Input { definitions: definitions.clone(), item_type: (!exchange).then_some(3), quantity: 1, credit: (!exchange).then_some((1, 1)), issuer: 3, chips: Arc::new(chips) };
        assert!(policy::plan(&pin, input, Instant::now() + Duration::from_secs(30)).is_err(), "accepted {result}");
    }
}

#[test]
fn closed_policy_obeys_caller_deadline_before_and_during_execution() {
    use std::time::{Duration, Instant};
    let lua = mlua::Lua::new();
    let config = crate::config::lua::LuaConfig::default();
    assert!(super::super::policy::run_closed(&lua, "return true", &config, (), Some(Instant::now() - Duration::from_secs(1))).is_err());
    assert!(super::super::policy::run_closed(&lua, "while true do end", &config, (), Some(Instant::now() + Duration::from_millis(1))).is_err());
}
