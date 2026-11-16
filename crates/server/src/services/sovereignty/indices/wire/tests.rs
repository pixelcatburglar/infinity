//! Constructor captures use synthetic values, not a running system or client data.
use super::*;
use crate::services::value_field;

#[tokio::test]
#[ignore = "requires capture source metadata environment (INFINITY_SOV_INDICES_CAPTURE_DIR and INFINITY_SOV_INDICES_CAPTURE_SOURCE), dedicated INFINITY_TEST_DATABASE_URL with SDE; run this exact test in its own process"]
async fn export_sovereignty_index_client_samples() {
    let directory = std::env::var("INFINITY_SOV_INDICES_CAPTURE_DIR").expect("capture directory");
    let source: serde_json::Value = serde_json::from_str(&std::env::var("INFINITY_SOV_INDICES_CAPTURE_SOURCE").expect("capture source")).expect("capture source JSON");
    assert!(source.is_object(), "capture source must be an object");
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut ids: Vec<_> = sde.map.systems().take(2).map(|system| system.solar_system_id).collect();
    ids.sort_unstable();
    let system_id = ids[0];
    let world = crate::space::World::new(db.clone(), sde.clone(), 0);
    let mut config = crate::config::Config::load().unwrap();
    let scripts = std::env::temp_dir().join(format!("indices-capture-{}-{}", std::process::id(), crate::handshake::random_u64()));
    std::fs::create_dir_all(scripts.join("content/sovereignty_indices/data")).unwrap();
    for (target, source) in [
        ("sovereignty_indices.lua", include_str!("../../../../../lua/sovereignty_indices.lua")),
        ("content/sovereignty_indices/policy.lua", include_str!("../../../../../lua/content/sovereignty_indices/policy.lua")),
        ("content/sovereignty_indices/data/policy.json", include_str!("../../../../../lua/content/sovereignty_indices/data/policy.json")),
    ] { std::fs::write(scripts.join(target), source).unwrap(); }
    config.lua.enabled = true;
    config.lua.scripts_dir = scripts.to_string_lossy().into_owned();
    config.lua.mod_dirs.clear();
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let entry = scripts.join("sovereignty_indices.lua");
    let disabled = scripts.join("sovereignty_indices.disabled");
    std::fs::rename(&entry, &disabled).unwrap();
    crate::lua::initialize_content(&config.lua, &db, &sde).await.unwrap();
    let missing_consumer = crate::lua::start(&config, &db, &sde, &world, &hub).await;
    std::fs::rename(&disabled, &entry).unwrap();
    let error = missing_consumer.err().expect("missing indices consumer must be refused");
    assert!(error.to_string().contains("configured indices bundle has no registered consumer"));
    crate::lua::start(&config, &db, &sde, &world, &hub).await.unwrap();
    let pin = crate::lua::content_policy("sovereignty_indices").unwrap();
    let compiled = pin.indices().unwrap();
    use std::sync::Arc;
    use crate::dao::sovereignty::{structures::{HubRow, StructureRow}, systems::SystemRow};
    use crate::services::sovereignty::indices::{activation::{PolicyHistory, PolicySegment}, rules};
    let now = crate::client::filetime_now();
    let claim_time = now - infinity_core::consts::FILETIME_DAY / 2;
    let history = PolicyHistory { identities: Arc::new(vec![]), segments: vec![PolicySegment {
        ordinal: 1, starts_at: 1, definitions: compiled.clone(),
    }] };
    let claimed = SystemRow { system_id, hub_item_id: Some(100), claim_corporation_id: Some(103),
        alliance_id: Some(104), claim_time: Some(claim_time), ownership_epoch: 1, revision: 1 };
    let mut v = rules::project(None, Some(&claimed), None, &history, system_id, now).unwrap();
    v.military_points = compiled.military[0].max_points + 0.5; v.industrial_points = compiled.industrial[0].max_points + 0.25; v.military_increasing = true;
    let mut zero = v.clone();
    zero.military_points = 0.0; zero.industrial_points = 0.0;
    zero.strategic_seconds = 0.0; zero.military_increasing = false; zero.strategic_increasing = false;
    let all_views = [v.clone()];
    let mut lost = v.clone();
    lost.strategic_seconds = 0.0; lost.strategic_increasing = false;
    let system = SystemRow { system_id, hub_item_id: Some(100), claim_corporation_id: None,
        alliance_id: None, claim_time: None, ownership_epoch: 1, revision: 1 };
    let hub = HubRow { system, structure: StructureRow { item_id: 100, system_id, source_item_id: 101,
        deployer_character_id: 102, deployer_corporation_id: 103, deployed_at: 1, content_namespace: "test".into(),
        content_version: "test".into(), self_destruct_due_at: None, self_destruct_actor: None },
        type_id: 1, owner_id: 103, x: None, y: None, z: None };
    let structure = crate::services::sovereignty::wire::structures;
    let available = structure(std::slice::from_ref(&hub), &[], Some(std::slice::from_ref(&zero))).unwrap();
    let unavailable = structure(std::slice::from_ref(&hub), &[], None).unwrap();
    let mut bad = zero.clone(); bad.defense_multiplier = f64::NAN;
    assert!(structure(std::slice::from_ref(&hub), &[], Some(&[bad])).is_err());
    let Value::Dict(entries) = development_system(&v, compiled) else { panic!("expected dict") };
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].0, Value::Int(compiled.military_attribute));
    assert_eq!(value_field(&entries[0].1, "points"), Some(&Value::Float(v.military_points)));
    assert_eq!(value_field(&entries[0].1, "increasing"), Some(&Value::Bool(true)));
    let Value::Dict(zero_entries) = development_system(&zero, compiled) else { panic!("expected dict") };
    assert_eq!(zero_entries.len(), 3);
    assert!(zero_entries.iter().all(|(_, entry)| value_field(entry, "points") == Some(&Value::Float(0.0))));
    let Value::List(rows) = development_all(&all_views) else { panic!("expected list") };
    assert_eq!(value_field(&rows[0], "claimedFor"), Some(&Value::Float(0.5)));
    let records = [
        ("system", "system", development_system(&v, compiled)),
        ("all", "all", development_all(&all_views)),
        ("zero", "system", development_system(&zero, compiled)),
        ("claim_loss", "system", development_system(&lost, compiled)),
        ("structure_available", "structure", Value::List(available)),
        ("structure_unavailable", "structure", Value::List(unavailable)),
    ];
    let records: Vec<_> = records.into_iter().map(|(name, kind, value)| serde_json::json!({
        "id": name, "kind": kind, "scenario": name, "payload_scope": "inner reply value; production constructor",
        "name": name, "source": "production constructor", "marshal": marshal::dumps(&value)
    })).collect();
    use crate::dao::sovereignty::{development::DevelopmentRow, policy::PolicyRow};
    let axes = [
        ("military", compiled.military.iter().map(|band| band.max_points).collect::<Vec<_>>()),
        ("industrial", compiled.industrial.iter().map(|band| band.max_points).collect()),
        ("strategic", compiled.strategic_seconds.iter().map(|seconds| *seconds as f64).collect()),
    ];
    let mut levels = Vec::new();
    for (axis, bounds) in axes {
        let mut points = vec![-1.0, 0.0];
        for bound in &bounds { points.extend([bound - 0.5, *bound, bound + 0.5]); }
        for points in points {
            if points < 0.0 || (axis != "strategic" && points > *bounds.last().unwrap()) {
                let reason = if points < 0.0 { "negative points" } else { "above policy cap" };
                levels.push(serde_json::json!({"axis": axis, "points": points, "server_projected": false, "reason": reason}));
                continue;
            }
            let row = DevelopmentRow {
                system_id, military_points: if axis == "military" { points } else { 0.0 },
                industrial_points: if axis == "industrial" { points } else { 0.0 },
                evaluated_at: now, revision: 1, activation_ordinal: 1,
                trend_start_at: (now - now.rem_euclid(compiled.decay_interval_ticks)).max(1),
                military_trend_base: 0.0, industrial_trend_base: 0.0, tail: None,
            };
            let mut strategic_system = claimed.clone();
            if axis == "strategic" {
                let ticks = (points * infinity_core::consts::FILETIME_SECOND as f64) as i64;
                strategic_system.claim_time = Some(now.checked_sub(ticks).expect("strategic claim time"));
            }
            let projected = rules::project(Some(&row), (axis == "strategic").then_some(&strategic_system),
                None, &history, system_id, now).unwrap();
            let level = match axis {
                "military" => projected.military,
                "industrial" => projected.industrial,
                _ => projected.strategic,
            };
            levels.push(serde_json::json!({"axis": axis, "points": points, "server_projected": true,
                "level": level.level, "remainder": level.remainder}));
        }
    }
    let mut multipliers = Vec::new();
    for military in 0..6 { for industrial in 0..6 { for strategic in 0..6 { for capital in [false, true] {
        let age = if strategic == 0 { 0 } else { compiled.strategic_seconds[strategic - 1] };
        let mut system = hub.system.clone();
        system.claim_corporation_id = Some(103); system.alliance_id = Some(104);
        system.claim_time = Some(now - age * infinity_core::consts::FILETIME_SECOND);
        let row = DevelopmentRow { system_id, military_points: compiled.military[military].min_points,
            industrial_points: compiled.industrial[industrial].min_points, evaluated_at: now, revision: 1,
            activation_ordinal: 1, trend_start_at: now - now.rem_euclid(compiled.decay_interval_ticks),
            military_trend_base: 0.0, industrial_trend_base: 0.0, tail: None };
        let policy = PolicyRow { alliance_id: 104, current_capital_system: capital.then_some(system_id),
            last_capital_changed_at: None, capital_lost_at: None, capital_change_not_before: None, revision: 1 };
        let projected = rules::project(Some(&row), Some(&system), Some(&policy), &history, system_id, now).unwrap();
        multipliers.push(serde_json::json!({"military":projected.military.level, "industrial":projected.industrial.level,
            "strategic":projected.strategic.level, "capital":capital, "defense_multiplier":projected.defense_multiplier}));
    }}}}
    let data = serde_json::json!({"consumer": "sov_development_indices", "source": source, "coverage": "constructor only; not runtime", "system_id": system_id,
        "neutral_system_id": ids[1],
        "military_points": v.military_points, "industrial_points": v.industrial_points, "claimed_days": 0.5, "now": now, "claim_time": claim_time,
        "invalid_structure": "rejected", "policy_version": pin.version(), "definitions": compiled.as_ref(),
        "levels": levels, "multipliers": multipliers, "records": records});
    std::fs::write(std::path::Path::new(&directory).join("sovereignty-index-samples.json"), serde_json::to_vec_pretty(&data).unwrap()).unwrap();
    std::fs::remove_dir_all(scripts).unwrap();
}
