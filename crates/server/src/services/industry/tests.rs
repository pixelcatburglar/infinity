use super::*;

#[test]
fn facility_services_keep_activity_order() {
    let mut facility = Facility { id: 1, type_id: 1, owner: 1, system: 1, security: 0.0, factory: false, laboratory: false };
    assert!(facility.activities().is_empty());
    facility.factory = true;
    assert_eq!(facility.activities(), vec![MANUFACTURING]);
    facility.factory = false;
    facility.laboratory = true;
    assert_eq!(facility.activities(), vec![RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION]);
    facility.factory = true;
    assert_eq!(facility.activities(), vec![MANUFACTURING, RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION]);
}

#[test]
fn facility_data_exposes_configured_tax_and_activity_index() {
    crate::tests::init_config();
    let facility = Facility { id: 1, type_id: 1, owner: 1, system: 1, security: 0.0, factory: true, laboratory: false };
    let data = facility.data();
    let policy = &crate::config::game().industry;
    assert_eq!(data.get("tax").and_then(Value::as_float), Some(policy.npc_facility_tax));
    let system = data.get("globalModifiers").and_then(|v| v.get("system")).unwrap();
    let Value::Dict(indices) = system else { panic!("system modifiers must be a dictionary") };
    assert!(indices.iter().any(|(activity, index)| activity.as_int() == Some(MANUFACTURING as i64) && index.as_float() == Some(policy.system_cost_index)));
    let Value::Instance(class, settings) = facility.tax_settings() else { panic!("expected KeyVal") };
    assert_eq!(*class, Value::str("utillib.KeyVal"));
    assert_eq!(settings.get("taxCorporation"), data.get("tax"));
    for name in ["taxAlliance", "taxStandingsHorrible", "taxStandingsBad",
        "taxStandingsNeutral", "taxStandingsGood", "taxStandingsHigh"] {
        assert_eq!(settings.get(name), Some(&Value::None));
    }
}

fn station_without_services(type_id: Option<i64>, solar_system_id: Option<i64>) -> crate::sde::map::Station {
    crate::sde::map::Station {
        station_id: 1, type_id, owner_id: None, solar_system_id, constellation_id: None,
        region_id: None, operation_id: None, x: None, y: None, z: None, security: None,
    }
}

#[test]
#[should_panic(expected = "SDE industry station has no type_id")]
fn station_without_services_requires_type_before_filtering() {
    Facility::from_station_services(&station_without_services(None, None), false, false);
}

#[test]
#[should_panic(expected = "SDE industry station has no solar_system_id")]
fn station_without_services_requires_system_before_filtering() {
    Facility::from_station_services(&station_without_services(Some(1), None), false, false);
}

mod corporation;

pub(super) async fn database_fingerprint(db: &crate::db::Db) -> Vec<Vec<String>> {
    let mut result = Vec::new();
    for table in ["accounts", "characters", "corporations", "corp_titles", "corp_wallets", "offices", "skills", "items", "blueprints", "industry_jobs", "market_orders", "market_order_history", "market_transactions", "trades", "trade_items", "jobs"] {
        result.push(crate::services::drones::tests::fixture::table_fingerprint(db, table).await.unwrap());
    }
    result
}
