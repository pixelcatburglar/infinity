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

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn large_personal_owner_receives_industry_notifications() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let location = crate::services::tests::fixture::default_location(crate::tests::shared_sde(&db).await.as_ref());
    let character = 1_500_000_124_i64;
    let job = Job { id: 1, installer: character, owner: character, activity: MANUFACTURING, blueprint_id: 2, blueprint_type: 1,
        blueprint_copy: false, facility: 3, system: 4, runs: 1, licensed_runs: 1, successful_runs: None, product_type: None,
        optional_type: None, cost: 0.0, probability: 1.0, time: 0, start: 0, end: 0, completed: None, completed_by: None,
        status: INDUSTRY_JOB_STATUS_READY, input_location: 5, input_flag: infinity_core::consts::FLAG_HANGAR, output_location: 6,
        output_flag: infinity_core::consts::FLAG_HANGAR, wallet_owner: character, wallet_account_key: infinity_core::consts::ACCOUNT_CASH,
        blueprint_location: 7, blueprint_flag: infinity_core::consts::FLAG_HANGAR };
    let result: Result<(), CallError> = db.transaction(|tx, _: &mut ()| { let job = job.clone(); Box::pin(async move {
        crate::services::tests::fixture::create_with_character_id(tx, character, location).await?;
        let mut outbox = Outbox::<()>::default();
        tell_job(tx, &mut outbox, &job).await?;
        let observed: Vec<_> = outbox.into_entries().into_iter().map(|entry| match entry {
            crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { char_id, event, args }) => (char_id, event, args),
            _ => (0, String::new(), Vec::new()),
        }).collect();
        let expected = vec![
            (character, "OnIndustryJob".to_string(), vec![Value::Int(1), Value::Int(character), Value::Int(2), Value::Int(character), Value::Int(i64::from(INDUSTRY_JOB_STATUS_READY)), Value::None]),
            (character, "OnBlueprintsUpdated".to_string(), vec![Value::Int(character)]),
        ];
        if observed != expected { return Err(CallError::Internal(anyhow::anyhow!("industry owner notifications differed"))); }
        Err(CallError::Internal(anyhow::anyhow!("owner fixture rollback")))
    }) }, |(), ()| async { unreachable!("rolled-back owner fixture must not dispatch") }).await;
    let Err(CallError::Internal(error)) = result else { panic!("owner fixture must roll back") };
    assert_eq!(error.to_string(), "owner fixture rollback");
    assert!(db.query_opt("SELECT character_id FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap().is_none());
}
