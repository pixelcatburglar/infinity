//! Corporation application RPC results and notification recipients.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn insertion_refusals_report_exact_errors_without_rows() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, manager.character, 1).await.unwrap();
    let deleted = fixture::create_corporation(&db, applicant.character, 1).await.unwrap();
    db.execute("UPDATE game.corporations SET deleted=true WHERE corporation_id=$1", &[&deleted]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        let missing: i64 = test_db.query_one("SELECT GREATEST(COALESCE(max(corporation_id), 98000000), 98000000) + 1000000 AS id FROM game.corporations", &[]).await.unwrap().get("id");
        let cases = [
            (missing, crate::messages::CORP_NOT_FOUND),
            (deleted, crate::messages::CORP_NOT_FOUND),
            (corp, crate::messages::CORP_NOT_RECRUITING),
            (1, crate::messages::NPC_CORP_DOES_NOT_ACCEPT_APPLICATIONS),
        ];
        for (target, message) in cases {
            let error = rpc(&mut client, "InsertApplication", &[Value::Int(target), Value::unicode("申请")], 1).await.unwrap_err();
            match error {
                crate::services::CallError::User { msg, dict } => {
                    assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
                    assert_eq!(dict, vec![("notify".to_owned(), Value::unicode(message))]);
                }
                other => panic!("expected UserError, got {other:?}"),
            }
            let count: i64 = test_db.query_one("SELECT count(*) AS n FROM game.corp_applications WHERE character_id=$1", &[&applicant.character]).await.unwrap().get("n");
            assert_eq!(count, 0);
        }
        fixture::assign_corporation(&test_db, applicant.character, corp, 0).await.unwrap();
        let error = rpc(&mut client, "InsertApplication", &[Value::Int(corp), Value::unicode("申请")], corp).await.unwrap_err();
        match error {
            crate::services::CallError::User { msg, dict } => {
                assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".to_owned(), Value::unicode(crate::messages::CORP_APPLICATION_ALREADY_PROCESSED))]);
            }
            other => panic!("expected UserError, got {other:?}"),
        }
        let count: i64 = test_db.query_one("SELECT count(*) AS n FROM game.corp_applications WHERE character_id=$1", &[&applicant.character]).await.unwrap().get("n");
        assert_eq!(count, 0);
    }).await;
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&deleted]).await.unwrap();
    cleanup(&db, corp, &[manager, applicant]).await;
    result.unwrap();
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn inserts_refuse_duplicates_and_notify_managers_and_applicant() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let second_manager = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let invitee = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, manager.character, 1).await.unwrap();
    fixture::assign_corporation(&db, manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, second_manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, member.character, corp, 0).await.unwrap();
    db.execute("UPDATE game.corporations SET is_recruiting=true WHERE corporation_id=$1", &[&corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        let (app_tx, mut app_rx) = tokio::sync::mpsc::unbounded_channel();
        let (mgr_tx, mut mgr_rx) = tokio::sync::mpsc::unbounded_channel();
        let (mgr2_tx, mut mgr2_rx) = tokio::sync::mpsc::unbounded_channel();
        let (member_tx, mut member_rx) = tokio::sync::mpsc::unbounded_channel();
        let (invitee_tx, mut invitee_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(applicant.character, app_tx);
        client.world.register_pilot(manager.character, mgr_tx);
        client.world.register_pilot(second_manager.character, mgr2_tx);
        client.world.register_pilot(member.character, member_tx);
        client.world.register_pilot(invitee.character, invitee_tx);
        let args = [Value::Int(corp), Value::unicode("申请")];
        test_db.execute("UPDATE game.corporations SET is_recruiting=false WHERE corporation_id=$1", &[&corp]).await.unwrap();
        assert!(rpc(&mut client, "InsertApplication", &args, 1).await.is_err());
        assert!(rpc(&mut client, "InsertApplication", &[Value::Int(1), Value::unicode("申请")], 1).await.is_err());
        test_db.execute("UPDATE game.corporations SET is_recruiting=true WHERE corporation_id=$1", &[&corp]).await.unwrap();
        assert_eq!(rpc(&mut client, "InsertApplication", &args, 1).await.unwrap(), Value::None);
        assert!(rpc(&mut client, "InsertApplication", &args, 1).await.is_err());
        let row = test_db.query_one("SELECT application_id,created FROM game.corp_applications WHERE corporation_id=$1 AND character_id=$2", &[&corp, &applicant.character]).await.unwrap();
        let id: i64 = row.get("application_id");
        let created: i64 = row.get("created");
        let expected = |status, message: &str| vec![Value::Int(corp), Value::Int(applicant.character), Value::Int(id), crate::services::keyval([
            ("applicationID", Value::Int(id)), ("corporationID", Value::Int(corp)),
            ("characterID", Value::Int(applicant.character)), ("applicationText", Value::unicode("申请")),
            ("status", Value::Int(status)), ("applicationDateTime", Value::Int(created)),
            ("customMessage", Value::unicode(message)),
        ])];
        for (rx, notices) in [(&mut app_rx, 0), (&mut mgr_rx, 1), (&mut mgr2_rx, 1)] {
            let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            assert_eq!(events.iter().filter(|e| e.0 == "OnNotificationReceived").count(), notices);
            if notices == 1 {
                assert!(events.iter().position(|e| e.0 == "OnCorporationApplicationChanged").unwrap() < events.iter().position(|e| e.0 == "OnNotificationReceived").unwrap());
            }
            let applications: Vec<_> = events.into_iter().filter(|e| e.0 == "OnCorporationApplicationChanged").collect();
            assert_eq!(applications, vec![("OnCorporationApplicationChanged".to_owned(), expected(0, ""))]);
        }
        assert!(member_rx.try_recv().is_err());
        client.session.apply(vec![("charid", Value::Int(manager.character)), ("corpid", Value::Int(corp))]);
        assert_eq!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 6), corp).await.unwrap(), Value::None);
        for (rx, notices) in [(&mut app_rx, 1), (&mut mgr_rx, 0), (&mut mgr2_rx, 0)] {
            let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            assert_eq!(events.iter().filter(|e| e.0 == "OnNotificationReceived").count(), notices);
            if notices == 1 {
                assert!(events.iter().position(|e| e.0 == "OnCorporationApplicationChanged").unwrap() < events.iter().position(|e| e.0 == "OnNotificationReceived").unwrap());
            }
            let applications: Vec<_> = events.into_iter().filter(|e| e.0 == "OnCorporationApplicationChanged").collect();
            assert_eq!(applications, vec![("OnCorporationApplicationChanged".to_owned(), expected(6, "response"))]);
        }
        client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(1))]);
        let mut update = offer(id, applicant.character, corp, 3);
        update[5] = Value::None;
        assert_eq!(rpc(&mut client, "UpdateApplicationOffer", &update, 1).await.unwrap(), Value::None);
        for (rx, notices) in [(&mut app_rx, 0), (&mut mgr_rx, 1), (&mut mgr2_rx, 1)] {
            let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            assert_eq!(events.iter().filter(|e| e.0 == "OnNotificationReceived").count(), notices);
            if notices == 1 {
                assert!(events.iter().position(|e| e.0 == "OnCorporationApplicationChanged").unwrap() < events.iter().position(|e| e.0 == "OnNotificationReceived").unwrap());
            }
            let applications: Vec<_> = events.into_iter().filter(|e| e.0 == "OnCorporationApplicationChanged").collect();
            assert_eq!(applications, vec![("OnCorporationApplicationChanged".to_owned(), expected(3, "response"))]);
        }
        assert!(member_rx.try_recv().is_err());
        let empty_id = application(&test_db, corp, applicant.character, 6).await;
        test_db.execute("UPDATE game.corp_applications SET custom_message='previous' WHERE application_id=$1", &[&empty_id]).await.unwrap();
        let mut empty = offer(empty_id, applicant.character, corp, 7);
        empty[5] = Value::unicode("");
        assert_eq!(rpc(&mut client, "UpdateApplicationOffer", &empty, 1).await.unwrap(), Value::None);
        assert_eq!(test_db.query_one("SELECT custom_message FROM game.corp_applications WHERE application_id=$1", &[&empty_id]).await.unwrap().get::<_, String>("custom_message"), "previous");
        for (rx, notices) in [(&mut app_rx, 0), (&mut mgr_rx, 1), (&mut mgr2_rx, 1)] {
            let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            let names: Vec<_> = events.iter().filter(|e| e.0 == "OnCorporationApplicationChanged" || e.0 == "OnNotificationReceived").map(|e| e.0.as_str()).collect();
            let expected = if notices == 1 { vec!["OnCorporationApplicationChanged", "OnNotificationReceived"] } else { vec!["OnCorporationApplicationChanged"] };
            assert_eq!(names, expected);
        }
        client.session.apply(vec![("charid", Value::Int(member.character)), ("corpid", Value::Int(corp))]);
        assert!(rpc(&mut client, "InsertInvitation", &[Value::Int(invitee.character)], corp).await.is_err());
        client.session.apply(vec![("charid", Value::Int(manager.character)), ("corpid", Value::Int(corp))]);
        assert_eq!(rpc(&mut client, "InsertInvitation", &[Value::Int(invitee.character)], corp).await.unwrap(), Value::None);
        assert!(rpc(&mut client, "InsertInvitation", &[Value::Int(invitee.character)], corp).await.is_err());
        assert_eq!(test_db.query_one("SELECT count(*) AS n FROM game.corp_applications WHERE corporation_id=$1 AND character_id=$2", &[&corp, &invitee.character]).await.unwrap().get::<_, i64>("n"), 1);
        let row = test_db.query_one("SELECT application_id,created FROM game.corp_applications WHERE corporation_id=$1 AND character_id=$2", &[&corp, &invitee.character]).await.unwrap();
        let id: i64 = row.get("application_id");
        let created: i64 = row.get("created");
        let invitation = ("OnCorporationApplicationChanged".to_owned(), vec![
            Value::Int(corp), Value::Int(invitee.character), Value::Int(id),
            crate::services::keyval([
                ("applicationID", Value::Int(id)), ("corporationID", Value::Int(corp)),
                ("characterID", Value::Int(invitee.character)), ("applicationText", Value::unicode("")),
                ("status", Value::Int(8)), ("applicationDateTime", Value::Int(created)),
                ("customMessage", Value::unicode("")),
            ]),
        ]);
        for (rx, notices) in [(&mut mgr_rx, 0), (&mut mgr2_rx, 0), (&mut invitee_rx, 1)] {
            let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            assert_eq!(events.iter().filter(|e| e.0 == "OnNotificationReceived").count(), notices);
            if notices == 1 {
                assert!(events.iter().position(|e| e.0 == "OnCorporationApplicationChanged").unwrap() < events.iter().position(|e| e.0 == "OnNotificationReceived").unwrap());
            }
            let applications: Vec<_> = events.into_iter().filter(|e| e.0 == "OnCorporationApplicationChanged").collect();
            assert_eq!(applications, vec![invitation.clone()]);
        }
        assert!(member_rx.try_recv().is_err());
        assert!(app_rx.try_recv().is_err());
    }).await;
    cleanup(&db, corp, &[manager, second_manager, applicant, invitee, member]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn successful_legacy_admission_persists_trained_capacity() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        let add = client.sde.dogma.effects_by_name("corporationMemberLimitBonus").next().unwrap().effect_id;
        let multiply = client.sde.dogma.effects_by_name("corporationManagementSkillBoostCorporationMemberBonus").next().unwrap().effect_id;
        let bonus = client.sde.dogma.attributes_by_name("corporationMemberBonus").next().unwrap();
        let base = client.sde.dogma.attributes_by_name("corporationMemberLimit").next().unwrap().default_value.unwrap();
        let skill = client.sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == add)
            && ty.effects().iter().any(|e| e.effect_id == multiply)
            && ty.attribute(bonus.attribute_id).or(bonus.default_value).is_some_and(|v| base + v * 5.0 > 1.0)).unwrap();
        let increment = skill.attribute(bonus.attribute_id).or(bonus.default_value).unwrap();
        let level = 5_i32;
        test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=$3", &[&ceo.character, &skill.type_id, &level]).await.unwrap();
        let id = application(&test_db, corp, applicant.character, 6).await;
        assert_eq!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), 1).await.unwrap(), Value::None);
        let saved: Option<i64> = test_db.query_one("SELECT member_limit FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap().get("member_limit");
        assert_eq!(saved, Some((base + increment * f64::from(level)) as i64));
        assert_eq!(corp_of(&test_db, applicant.character).await, corp);
        assert_eq!(status(&test_db, id).await, 2);
    }).await;
    cleanup(&db, corp, &[ceo, applicant]).await;
    result.unwrap();
}
