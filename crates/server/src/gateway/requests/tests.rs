use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn claimed_unauthorized_does_not_fall_back() {
    let req = Request {
        issued: None, correlation_uuid: vec![1; 16], external_origin: String::new(),
        application_instance_uuid: vec![2; 16], authoritative_context: None,
        payload: Some(prost_types::Any { type_url: "type.googleapis.com/eve_public.character.GetNamesByUserRequest".into(), value: vec![] }),
    };
    let gateway = super::super::tests::gateway().await;
    let response = handle_request(gateway.as_ref(), req, None).await;
    assert_eq!(response.status_code, 401);
    assert_eq!(response.status_message, text::UNAUTHORIZED);
    assert!(response.payload.is_none());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn invalid_payload_does_not_expose_diagnostics() {
    let req = Request {
        issued: None, correlation_uuid: vec![1; 16], external_origin: String::new(),
        application_instance_uuid: vec![2; 16], authoritative_context: None,
        payload: Some(prost_types::Any { type_url: "type.googleapis.com/eve_public.character.GetNamesByUserRequest".into(), value: vec![0xff] }),
    };
    let gateway = super::super::tests::gateway().await;
    let response = handle_request(gateway.as_ref(), req, None).await;
    assert_eq!(response.status_code, 400);
    assert_eq!(response.status_message, text::INVALID_REQUEST);
}


#[test]
fn refusals_return_safe_status_and_no_payload() {
    let name = "eve_public.character.skill.plan.DeleteRequest";
    for (code, expected) in [(FailureCode::BadRequest, 400), (FailureCode::Unauthorized, 401), (FailureCode::NotImplemented, 501)] {
        let mut response = Response::default();
        apply_error(&mut response, name, GatewayError::refusal(code, text::INVALID_REQUEST));
        assert_eq!(response.status_code, expected);
        assert_eq!(response.status_message, text::INVALID_REQUEST);
        assert!(response.payload.is_none());
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn unclaimed_matrix_retains_default_response() {
    let name = "eve_public.matrix.api.GetEquippedRequest";
    let req = Request {
        issued: None, correlation_uuid: vec![1; 16], external_origin: String::new(),
        application_instance_uuid: vec![2; 16], authoritative_context: None,
        payload: Some(prost_types::Any { type_url: format!("type.googleapis.com/{name}"), value: vec![] }),
    };
    let gateway = super::super::tests::gateway().await;
    let response = handle_request(gateway.as_ref(), req, None).await;
    assert_eq!(response.status_code, 200);
    let payload = response.payload.unwrap();
    let desc = message(&response_name(name).unwrap()).unwrap();
    let decoded = DynamicMessage::decode(desc, payload.value.as_slice()).unwrap();
    assert!(format!("{decoded}").contains("no_equipped:true"));
}


#[test]
fn database_and_internal_errors_are_safe_on_the_wire() {
    let error: GatewayError = crate::db::Error::Pool(deadpool_postgres::PoolError::Closed).into();
    match &error {
        GatewayError::Database(source) => {
            assert!(matches!(source, crate::db::Error::Pool(deadpool_postgres::PoolError::Closed)));
            assert!(!source.to_string().is_empty());
        }
        other => panic!("lost database source: {other:?}"),
    }
    for error in [error, GatewayError::Internal(anyhow::anyhow!("private diagnostic"))] {
        let mut response = Response::default();
        apply_error(&mut response, "test", error);
        assert_eq!(response.status_code, 500);
        assert_eq!(response.status_message, text::INTERNAL_ERROR);
        assert!(response.payload.is_none());
    }
}

#[test]
fn unclaimed_content_requests_return_no_any() {
    let name = "eve_public.achievement.definition.api.cdn.GetLatestRequest";
    let desc = message(name).unwrap();
    assert!(desc.get_field_by_name("no_local_version_available").is_some());
    let payload = DynamicMessage::new(desc);
    let caller = Caller::default();
    let mut response = Response { status_code: 200, ..Default::default() };
    apply_unclaimed(&mut response, name, &payload, &caller);
    assert_eq!(response.status_code, 200);
    assert!(response.status_message.is_empty());
    assert!(response.payload.is_none());
}

#[test]
fn empty_response_any_is_distinct_from_absent_payload() {
    let name = "eve_public.character.skill.plan.DeleteRequest";
    let desc = message(&response_name(name).unwrap()).unwrap();
    let empty = DynamicMessage::new(desc.clone());
    let packed = pack(&empty);
    assert!(packed.value.is_empty());
    assert_eq!(type_name(&packed.type_url), desc.full_name());
    assert!(DynamicMessage::decode(desc, packed.value.as_slice()).is_ok());
    let with_payload = Response { payload: Some(packed), ..Default::default() };
    let without_payload = Response::default();
    assert!(with_payload.payload.is_some());
    assert!(without_payload.payload.is_none());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn forged_character_ownership_never_binds_notice_stream() {
    use crate::gateway::{notices::GatewayNotice, protocol::target_group::Target};
    use crate::gateway::protocol::{AuthoritativeContext, Identifier32, UserIdentifier};
    let gateway = super::super::tests::gateway().await;
    let account: i64 = gateway.db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('gateway-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let other: i64 = gateway.db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('gateway-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = gateway.db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'gateway-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    gateway.world.register_pilot(character, tx);
    gateway.world.set_online_session(character, Some(crate::session::OnlineSession {
        character_id: character, user_id: Some(account), session_id: 1, ship_id: None, station_id: None,
        system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let app = [1;16];
    let lease = gateway.notice_hub.lease(app);
    let mut stream = gateway.notice_hub.register(app, 1);
    let req = Request {
        issued: None, correlation_uuid: vec![1;16], external_origin: String::new(), application_instance_uuid: app.to_vec(),
        authoritative_context: Some(AuthoritativeContext { tenant: String::new(), authenticated_user: Some(UserIdentifier { sequential: i32::try_from(other).unwrap() }), active_character: Some(Identifier32 { sequential: u32::try_from(character).unwrap() }), identity: None }),
        payload: Some(prost_types::Any { type_url: "type.googleapis.com/eve_public.character.GetNamesByUserRequest".into(), value: Vec::new() }),
    };
    assert_eq!(handle_request(&gateway, req, Some(&lease)).await.status_code, 403);
    let payload = DynamicMessage::new(message("eve_public.Event").unwrap());
    gateway.notice_hub.dispatch(GatewayNotice::new(Target::Character(u32::try_from(character).unwrap()), &payload).unwrap(), |id| gateway.world.online_session(id));
    assert!(stream.rx.try_recv().is_err());
    gateway.world.unregister_pilot(character);
    gateway.db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap();
    gateway.db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    gateway.db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&other]).await.unwrap();
}
