//! Compile and retained-package behavior for the shipped fields-deployables
//! policy; the shipped Lua source decides the routing, client data is never
//! copied.

use super::*;

const SOURCE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/fields_deployables/policy.lua"));
const AUTHORED: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/fields_deployables/data/policy.json"));

#[test]
fn shipped_compile_routes_probe_groups_to_payload_families() {
    let definitions = compile(&Lua::new(), SOURCE, AUTHORED, &LuaConfig::default(), None).unwrap();
    // The published interdiction probe groups resolve to their families; an
    // unrelated group (scanner probes, group 479) has no routing.
    assert_eq!(definitions.payload_family(548), Some(PayloadFamily::WarpField));
    assert_eq!(definitions.payload_family(4088), Some(PayloadFamily::StasisPulse));
    assert_eq!(definitions.payload_family(479), None);
    assert_eq!(definitions.stasis_collection_id(), 27);
}

#[test]
fn invalid_authored_and_compiled_policies_are_rejected() {
    for data in [
        br#"{"version":"1","source":"t","payloads":{},"stasis_collection_id":27}"#.as_slice(),
        br#"{"version":"1","source":"t","payloads":{"548":"other"},"stasis_collection_id":27}"#.as_slice(),
        br#"{"version":"1","source":"t","payloads":{"548":"warp_field"},"stasis_collection_id":0}"#.as_slice(),
        br#"{"version":"1","source":"t","payloads":{"548":"warp_field","-4":"stasis_pulse"},"stasis_collection_id":27}"#.as_slice(),
    ] {
        assert!(compile(&Lua::new(), SOURCE, data, &LuaConfig::default(), None).is_err(), "{data:?}");
    }
    for source in [
        b"return {}".as_slice(),
        b"return { Compile = function() return 3 end }".as_slice(),
        b"return { Compile = function() return { payloads = {} } end }".as_slice(),
        b"return { Compile = function() return { payloads = { [548] = 'warp_field' } } end, Extra = function() end }".as_slice(),
    ] {
        assert!(compile(&Lua::new(), source, AUTHORED, &LuaConfig::default(), None).is_err());
    }
    // A referenced collection that does not exist in the SDE is refused; the
    // real shape check is the Minimum/PostPercent/maxVelocity rule.
    assert!(compile(&Lua::new(), SOURCE, AUTHORED, &LuaConfig::default(), Some(&crate::sde::Sde::default())).is_err());
}
#[test]
fn stasis_collection_must_be_the_published_web() {
    let row = |data: &str| crate::dao::sde::dbuff::Row { key: Some("27".into()), data: data.into() };
    let exact = r#"{"_key":27,"aggregateMode":"Minimum","operationName":"PostPercent","itemModifiers":[{"dogmaAttributeID":37}]}"#;
    let extra = r#"{"_key":27,"aggregateMode":"Minimum","operationName":"PostPercent","itemModifiers":[{"dogmaAttributeID":37},{"dogmaAttributeID":9}]}"#;
    let accepted = crate::sde::dbuff::fixture::collections(vec![row(exact)]).unwrap();
    let collection = accepted.get(27).unwrap();
    assert_eq!(collection.aggregate, crate::sde::dbuff::DbuffAggregate::Minimum);
    assert_eq!(collection.modifiers.len(), 1);
    assert_eq!(collection.modifiers[0].modifier.op, crate::services::fit::OP_POST_PERCENT);
    assert_eq!(collection.modifiers[0].modifier.modified, infinity_core::consts::dogma::ATTR_MAX_VELOCITY);
    let rejected = crate::sde::dbuff::fixture::collections(vec![row(extra)]).unwrap();
    assert_ne!(rejected.get(27).unwrap().modifiers.len(), 1);
    let mut sde = crate::sde::Sde::default();
    crate::sde::types::tests::add_group(&mut sde, 548);
    crate::sde::types::tests::add_group(&mut sde, 4088);
    sde.dbuffs = accepted;
    assert!(compile(&Lua::new(), SOURCE, AUTHORED, &LuaConfig::default(), Some(&sde)).is_ok());
}
#[test]
fn retained_package_recompiles_and_keeps_pinned_definitions() {
    let lua = Lua::new();
    let row = super::super::package_row(NAMESPACE.to_owned(), "1".to_owned(), Vec::new(), SOURCE.to_vec(), AUTHORED.to_vec()).unwrap();
    let package = super::super::build_package(&lua, row.clone(), &LuaConfig::default(), &crate::sde::Sde::default(),
        super::super::daily_goals::model::ValidationContext::Retained, None).unwrap();
    let definitions = package.fields_deployables().expect("fields domain");
    let pinned = std::sync::Arc::clone(definitions);
    assert_eq!(definitions.payload_family(548), Some(PayloadFamily::WarpField));
    assert_eq!(definitions.payload_family(4088), Some(PayloadFamily::StasisPulse));
    assert_eq!(definitions.stasis_collection_id(), 27);
    let _reloaded = compile(&Lua::new(), SOURCE, AUTHORED, &LuaConfig::default(), None).unwrap();
    assert_eq!(pinned.stasis_collection_id(), 27);
    // Tampered retained bytes fail the stored-identity checks instead of rebuilding.
    let mut tampered = (*row).clone();
    tampered.fingerprint = vec![0; 32];
    assert!(super::super::build_package(&lua, std::sync::Arc::new(tampered), &LuaConfig::default(),
        &crate::sde::Sde::default(), super::super::daily_goals::model::ValidationContext::Retained, None).is_err());
}
