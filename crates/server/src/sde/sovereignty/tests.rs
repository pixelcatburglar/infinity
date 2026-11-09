use super::*;
use serde_json::{json, Value};

fn row() -> dao::ComponentRow {
    dao::ComponentRow { key: Some(TYPE_SOVEREIGNTY_HUB.to_string()), data: json!({
        "_key": TYPE_SOVEREIGNTY_HUB,
        "physics": {"isAlwaysGlobal": 1},
        "deploy": {
            "defaultOwner": 42, "deployAtRange": 12.5, "requiresFleet": 1,
            "launchForOwnCorp": 1, "maxSecurityLevel": -0.5, "requiresAlliance": 0,
            "fixedStarDistance": 12345678901i64, "maxPerSolarSystem": 3,
            "disallowInVoidSpace": 1, "disallowInAbyssSpace": 0, "disallowIfCynoBlocked": 1,
            "disallowInWormholeSpace": 0, "isForPlayerOwnableSpace": 1,
            "minDistanceFromOwnGroup": 2.5, "disallowInTriglavianSpace": 0,
            "flagAsSuspectWhenDeployed": 1, "minDistanceFromControlTower": 4.5,
            "disallowInWarpDisruptedDungeon": 0, "minDistanceFromStargatesAndStations": 6.5
        },
        "entosisSovereigntyHub": {"campaignEventType": 7, "captureTimeSeconds": 17.5, "defensiveRegenRate": 0.4}
    }).to_string() }
}
fn mutate(r: &mut dao::ComponentRow, f: impl FnOnce(&mut Value)) {
    let mut data: Value = serde_json::from_str(&r.data).unwrap();
    f(&mut data);
    r.data = data.to_string();
}
fn parse(rows: Vec<dao::ComponentRow>) -> anyhow::Result<Sovereignty> {
    Sovereignty::from_rows(rows, Some(Some(GROUP_SOVEREIGNTY_HUB)))
}
fn error(row: dao::ComponentRow, name: &str) {
    let message = match parse(vec![row]) { Ok(_) => panic!("expected error for {name}"), Err(e) => format!("{e:#}") };
    assert!(message.contains(name), "{message}");
}

#[test]
fn valid_synthetic_and_absent_activate() {
    let result = parse(vec![row()]).unwrap();
    let hub = result.hub();
    assert!(hub.deploy.requires_fleet);
    assert!(hub.is_always_global);
    assert_eq!(hub.deploy.fixed_star_distance_m, 12345678901);
    assert_eq!(hub.deploy.min_distance_from_ess_m, None);
    assert_eq!(hub.capture.capture_time_seconds, 17.5);
}

#[test]
fn missing_deploy_and_capture_keys_fail() {
    for (component, key) in [("deploy", "defaultOwner"), ("deploy", "deployAtRange"), ("entosisSovereigntyHub", "captureTimeSeconds"), ("entosisSovereigntyHub", "defensiveRegenRate")] {
        let mut r = row();
        mutate(&mut r, |data| { data[component].as_object_mut().unwrap().remove(key); });
        error(r, key);
    }
    let mut r = row();
    mutate(&mut r, |data| { data.as_object_mut().unwrap().remove("deploy"); });
    error(r, "deploy");
}

#[test]
fn flags_and_types_are_checked() {
    for bad in [json!(2), json!(-1), json!(true), json!("1"), Value::Null] {
        let mut r = row(); mutate(&mut r, |data| data["deploy"]["requiresFleet"] = bad); error(r, "requiresFleet");
    }
    let mut r = row(); mutate(&mut r, |data| data["deploy"]["fixedStarDistance"] = json!(1.5)); error(r, "fixedStarDistance");
    let mut r = row(); mutate(&mut r, |data| data["deploy"]["minDistanceFromESS"] = Value::Null); error(r, "minDistanceFromESS");
    let mut r = row(); mutate(&mut r, |data| data["activate"] = json!({})); error(r, "activate");
    let mut r = row(); mutate(&mut r, |data| data["physics"]["isAlwaysGlobal"] = json!(2)); error(r, "isAlwaysGlobal");
}

#[test]
fn unused_source_fields_are_still_validated() {
    for key in ["disallowInVoidSpace", "disallowInAbyssSpace", "disallowInWormholeSpace", "isForPlayerOwnableSpace", "disallowInTriglavianSpace"] {
        for value in [json!(0), json!(1)] {
            let mut r = row(); mutate(&mut r, |data| data["deploy"][key] = value);
            assert!(parse(vec![r]).is_ok());
        }
        for value in [json!(2), json!(true), Value::Null] {
            let mut r = row(); mutate(&mut r, |data| data["deploy"][key] = value); error(r, key);
        }
        let mut r = row(); mutate(&mut r, |data| { data["deploy"].as_object_mut().unwrap().remove(key); }); error(r, key);
    }
    for value in [json!(-1), json!(1.5), Value::Null] {
        let mut r = row(); mutate(&mut r, |data| data["entosisSovereigntyHub"]["campaignEventType"] = value); error(r, "campaignEventType");
    }
    let mut r = row(); mutate(&mut r, |data| { data["entosisSovereigntyHub"].as_object_mut().unwrap().remove("campaignEventType"); }); error(r, "campaignEventType");
}

#[test]
fn finite_and_range_checks() {
    for (component, key, bad) in [
        ("deploy", "maxSecurityLevel", json!(1.1)), ("deploy", "deployAtRange", json!(-1)),
        ("deploy", "fixedStarDistance", json!(-1)), ("deploy", "maxPerSolarSystem", json!(0)),
        ("entosisSovereigntyHub", "captureTimeSeconds", json!(0)),
        ("entosisSovereigntyHub", "defensiveRegenRate", json!(-0.1)),
        ("entosisSovereigntyHub", "captureTimeSeconds", Value::Null),
    ] {
        let mut r = row(); mutate(&mut r, |data| data[component][key] = bad); error(r, key);
    }
    assert!(number(&json!({"x": null}), "x").is_err());
}

#[test]
fn duplicates_conflicts_and_sde_references_fail() {
    assert!(parse(vec![]).err().unwrap().to_string().contains("missing"));
    assert!(parse(vec![row(), row()]).err().unwrap().to_string().contains("duplicate"));
    let mut changed = row(); mutate(&mut changed, |data| data["deploy"]["requiresFleet"] = json!(0));
    assert!(parse(vec![row(), changed]).err().unwrap().to_string().contains("conflicting"));
    let mut r = row(); r.key = Some("1".into()); error(r, "key");
    let mut r = row(); mutate(&mut r, |data| data["_key"] = json!(1)); error(r, "_key");
    assert!(Sovereignty::from_rows(vec![row()], None).err().unwrap().to_string().contains("type reference"));
    assert!(Sovereignty::from_rows(vec![row()], Some(Some(1))).err().unwrap().to_string().contains("group reference"));
    assert!(Sovereignty::from_rows(vec![row()], Some(None)).err().unwrap().to_string().contains("group reference"));
}

#[test]
fn malformed_data_has_named_context() {
    let mut r = row(); r.data = "{".into(); error(r, "spacecomponentsbytype.data");
}
