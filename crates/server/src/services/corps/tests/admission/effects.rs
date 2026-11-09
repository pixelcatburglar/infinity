//! Transactional corporation admission effects and persisted notifications.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn application_notices_persist_with_correct_recipients_and_payloads() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let second_manager = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, manager.character, 1).await.unwrap();
    fixture::assign_corporation(&db, manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, second_manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    db.execute("UPDATE game.corporations SET is_recruiting=true,member_limit=10 WHERE corporation_id=$1", &[&corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        let (manager_tx, mut manager_rx) = tokio::sync::mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
        let (applicant_tx, mut applicant_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(manager.character, manager_tx);
        client.world.register_pilot(second_manager.character, second_tx);
        client.world.register_pilot(applicant.character, applicant_tx);
        macro_rules! check_events {
            ($manager_notice:expr, $applicant_notice:expr) => {
                for (rx, notice) in [(&mut manager_rx, $manager_notice), (&mut second_rx, $manager_notice), (&mut applicant_rx, $applicant_notice)] {
                    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
                    let names: Vec<_> = events.iter().filter(|e| e.0 == "OnCorporationApplicationChanged" || e.0 == "OnNotificationReceived").map(|e| e.0.as_str()).collect();
                    let expected = if notice { vec!["OnCorporationApplicationChanged", "OnNotificationReceived"] } else { vec!["OnCorporationApplicationChanged"] };
                    assert_eq!(names, expected);
                }
            };
        }
        rpc(&mut client, "InsertApplication", &[Value::Int(corp), Value::unicode("申请")], 1).await.unwrap();
        assert!(rpc(&mut client, "InsertApplication", &[Value::Int(corp), Value::unicode("申请")], 1).await.is_err());
        check_events!(true, false);
        let id: i64 = test_db.query_one("SELECT application_id FROM game.corp_applications WHERE corporation_id=$1 AND character_id=$2", &[&corp, &applicant.character]).await.unwrap().get("application_id");
        client.session.apply(vec![("charid", Value::Int(manager.character)), ("corpid", Value::Int(corp))]);
        rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 6), corp).await.unwrap();
        check_events!(false, true);
        client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(1))]);
        rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 3), 1).await.unwrap();
        check_events!(true, false);
        for (from, to, message) in [(0, 4, ""), (0, 4, "拒绝\"\n原因"), (0, 7, ""), (6, 7, ""), (8, 7, "")] {
            let id = application(&test_db, corp, applicant.character, from).await;
            let manager_action = to == 4;
            let actor = if manager_action { manager.character } else { applicant.character };
            let bound = if manager_action { corp } else { 1 };
            client.session.apply(vec![("charid", Value::Int(actor)), ("corpid", Value::Int(bound))]);
            let mut args = offer(id, applicant.character, corp, to);
            args[5] = Value::unicode(message);
            rpc(&mut client, "UpdateApplicationOffer", &args, bound).await.unwrap();
            check_events!(!manager_action, manager_action);
        }
        client.session.apply(vec![("charid", Value::Int(manager.character)), ("corpid", Value::Int(corp))]);
        rpc(&mut client, "InsertInvitation", &[Value::Int(applicant.character)], corp).await.unwrap();
        check_events!(false, true);
        let invitation: i64 = test_db.query_one("SELECT application_id FROM game.corp_applications WHERE corporation_id=$1 AND character_id=$2 AND status=8", &[&corp, &applicant.character]).await.unwrap().get("application_id");
        client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(1))]);
        let mut args = offer(invitation, applicant.character, corp, 2);
        args[3] = Value::unicode("");
        rpc(&mut client, "UpdateApplicationOffer", &args, 1).await.unwrap();
        assert!(rpc(&mut client, "UpdateApplicationOffer", &args, corp).await.is_err());
        check_events!(true, false);
        let rows = test_db.query("SELECT receiver_id,type_id,group_id,sender_id,data FROM game.notifications WHERE receiver_id=ANY($1) ORDER BY notification_id", &[&vec![manager.character, second_manager.character, applicant.character]]).await.unwrap();
        let mut actual: Vec<(i32, i64)> = rows.iter().map(|r| (r.get("type_id"), r.get("receiver_id"))).collect();
        let mut expected = Vec::new();
        for kind in [16, 129, 130, 130, 130, 128] {
            expected.push((kind, manager.character));
            expected.push((kind, second_manager.character));
        }
        for kind in [18, 17, 142, 139] { expected.push((kind, applicant.character)); }
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        for row in rows {
            let kind: i32 = row.get("type_id");
            let to_manager = matches!(kind, 16 | 128 | 129 | 130);
            assert_eq!(row.get::<_, i64>("sender_id"), if to_manager { applicant.character } else { corp });
            assert_eq!(row.get::<_, i32>("group_id"), 3);
            let data: String = row.get("data");
            let json: serde_json::Value = serde_json::from_str(&data).unwrap();
            assert_eq!(json["corpID"], corp);
            if kind == 142 { assert_eq!(json["customMessage"], "拒绝\"\n原因"); }
        }
    }).await;
    cleanup(&db, corp, &[manager, second_manager, applicant]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn admission_session_precedes_member_events_and_late_mail_failure_rolls_back() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let old_manager = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, manager.character, 1).await.unwrap();
    let old = fixture::create_corporation(&db, old_manager.character, 1).await.unwrap();
    fixture::assign_corporation(&db, manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, old_manager.character, old, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, applicant.character, old, 0).await.unwrap();
    db.execute("UPDATE game.characters SET corp_title_mask=1,corp_title='test' WHERE character_id=$1", &[&applicant.character]).await.unwrap();
    let authority = crate::dao::characters::roles::authority(&db, applicant.character).await.unwrap().unwrap();
    assert_eq!(authority.roles, [0; 4]);
    assert_eq!(authority.grantable, [0; 4]);
    let (mut client, peer) = tests::client(db.clone(), applicant.character).await;
    let station = client.sde.map.stations().find(|station| station.solar_system_id.is_some()).unwrap();
    let station_id = station.station_id;
    let system_id = station.solar_system_id.unwrap();
    crate::dao::characters::ship::dock(&db, applicant.character, station_id, system_id).await.unwrap();
    db.execute("UPDATE game.corporations SET member_limit=10,welcome_mail='Welcome' WHERE corporation_id=$1", &[&corp]).await.unwrap();
    let id = application(&db, corp, applicant.character, 6).await;
    let constraint = format!("admission_mail_reject_{corp}");
    db.execute(&format!("ALTER TABLE game.mail_messages ADD CONSTRAINT {constraint} CHECK (sender_id <> {corp}) NOT VALID"), &[]).await.unwrap();
    let test_db = db.clone();
    let expected_constraint = constraint.clone();
    let result = tokio::spawn(async move {
        let _peer = peer;
        client.session.apply(vec![("corpid", Value::Int(old)), ("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system_id))]);
        let (app_tx, mut app_rx) = tokio::sync::mpsc::unbounded_channel();
        let (old_tx, mut old_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_tx, mut new_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(applicant.character, app_tx);
        client.world.register_pilot(old_manager.character, old_tx);
        client.world.register_pilot(manager.character, new_tx);
        match rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), old).await {
            Err(crate::services::CallError::Internal(error)) => match error.downcast_ref::<crate::db::Error>() {
                Some(crate::db::Error::Postgres(error)) => {
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                    assert_eq!(error.as_db_error().and_then(|e| e.constraint()), Some(expected_constraint.as_str()));
                }
                other => panic!("expected PostgreSQL constraint failure, got {other:?}"),
            },
            other => panic!("expected the fixture constraint failure, got {other:?}"),
        }
        assert_eq!(status(&test_db, id).await, 6);
        let row = test_db.query_one("SELECT corporation_id,corp_roles,corp_title_mask,corp_title FROM game.characters WHERE character_id=$1", &[&applicant.character]).await.unwrap();
        assert_eq!(row.get::<_, i64>("corporation_id"), old);
        assert_eq!(row.get::<_, i64>("corp_roles"), 0);
        assert_eq!(row.get::<_, i64>("corp_title_mask"), 1);
        assert_eq!(row.get::<_, String>("corp_title"), "test");
        let row = test_db.query_one("SELECT (SELECT count(*) FROM game.employment_history WHERE char_id=$1 AND corporation_id=$2) AS jobs, (SELECT count(*) FROM game.mail_messages WHERE sender_id=$2) AS mail, (SELECT count(*) FROM game.notifications WHERE receiver_id=ANY($3)) AS notices", &[&applicant.character, &corp, &vec![applicant.character, manager.character, old_manager.character]]).await.unwrap();
        for key in ["jobs", "mail", "notices"] { assert_eq!(row.get::<_, i64>(key), 0); }
        assert!(app_rx.try_recv().is_err());
        assert!(old_rx.try_recv().is_err());
        assert!(new_rx.try_recv().is_err());
    }).await;
    db.execute(&format!("ALTER TABLE game.mail_messages DROP CONSTRAINT {constraint}"), &[]).await.unwrap();
    let test_db = db.clone();
    let success = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        client.session.apply(vec![("corpid", Value::Int(old)), ("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system_id))]);
        let (app_tx, mut app_rx) = tokio::sync::mpsc::unbounded_channel();
        let (old_tx, mut old_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_tx, mut new_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(applicant.character, app_tx);
        client.world.register_pilot(old_manager.character, old_tx);
        client.world.register_pilot(manager.character, new_tx);
        rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), old).await.unwrap();
        let app: Vec<_> = std::iter::from_fn(|| app_rx.try_recv().ok()).collect();
        let old_events: Vec<_> = std::iter::from_fn(|| old_rx.try_recv().ok()).collect();
        let new_events: Vec<_> = std::iter::from_fn(|| new_rx.try_recv().ok()).collect();
        assert_eq!(app[0].0, "__session");
        assert!(app[0].1.contains(&Value::tuple([Value::str("corpAccountKey"), Value::Int(infinity_core::consts::ACCOUNT_CASH as i64)])));
        let change = Value::dict([("corporationID", Value::tuple([Value::Int(old), Value::Int(corp)]))]);
        let member = |audience| ("OnCorporationMemberChanged".to_owned(), vec![Value::Int(audience), Value::Int(applicant.character), change.clone()]);
        let member_events: Vec<_> = app.iter().filter(|e| e.0 == "OnCorporationMemberChanged").collect();
        assert_eq!(member_events.len(), 1);
        assert_eq!(member_events[0], &member(corp));
        assert!(old_events.contains(&member(old)));
        assert!(new_events.contains(&member(corp)));
        let count = |audience, before, after| ("OnCorporationChanged".to_owned(), vec![Value::Int(audience), Value::dict([("memberCount", Value::tuple([Value::Int(before), Value::Int(after)]))])]);
        assert!(old_events.contains(&count(old, 2, 1)));
        assert!(new_events.contains(&count(corp, 1, 2)));
        assert!(app.contains(&count(corp, 1, 2)));
    }).await;
    cleanup(&db, corp, &[manager, applicant]).await;
    cleanup(&db, old, &[old_manager]).await;
    result.unwrap();
    success.unwrap();
}
