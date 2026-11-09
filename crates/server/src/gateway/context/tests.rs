use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn ownership_and_offline_identity() {
    let gw = super::super::tests::gateway().await;
    let account: i64 = gw.db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('gateway-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = gw.db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'gateway-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let caller = Caller { user_id: Some(account), character_id: Some(character), ..Default::default() };
    let ctx = HandlerContext::for_request(&gw, caller.clone()).await.unwrap();
    assert_eq!(ctx.require_user().unwrap(), account);
    assert_eq!(ctx.require_character().unwrap().char_id, character);
    assert_eq!(ctx.identity.ship_id, None);
    assert_eq!(ctx.identity.station_id, None);
    assert_eq!(ctx.identity.system_id, None);
    assert!(matches!(HandlerContext::for_request(&gw, Caller { user_id: None, ..caller.clone() }).await, Err(GatewayError::Refused { code: FailureCode::Unauthorized, .. })));
    assert!(matches!(HandlerContext::for_request(&gw, Caller { user_id: Some(-1), ..caller }).await, Err(GatewayError::Refused { code: FailureCode::Unauthorized, .. })));
    gw.db.execute("UPDATE game.characters SET deleted = TRUE WHERE character_id = $1", &[&character]).await.unwrap();
    assert!(matches!(HandlerContext::for_request(&gw, caller.clone()).await, Err(GatewayError::Refused { code: FailureCode::Forbidden, .. })));
    gw.db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap();
    gw.db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn anonymous_context_requires_user_and_character() {
    let gw = super::super::tests::gateway().await;
    let ctx = HandlerContext::for_request(&gw, Caller::default()).await.unwrap();
    assert!(matches!(ctx.require_user(), Err(GatewayError::Refused { code: FailureCode::Unauthorized, .. })));
    assert!(matches!(ctx.require_character(), Err(GatewayError::Refused { code: FailureCode::Unauthorized, .. })));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn online_identity_and_generation_safe_unregister() {
    let gw = super::super::tests::gateway().await;
    let account: i64 = gw.db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('gateway-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = gw.db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'gateway-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    gw.world.register_pilot(character, tx);
    gw.world.set_online_session(character, Some(infinity_core::session::OnlineSession {
        character_id: character, user_id: Some(account), session_id: 2, ship_id: Some(20),
        station_id: Some(30), system_id: Some(40), corporation_id: Some(50),
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let caller = Caller { user_id: Some(account), character_id: Some(character), ..Default::default() };
    let ctx = HandlerContext::for_request(&gw, caller.clone()).await.unwrap();
    let identity = ctx.require_character().unwrap();
    assert_eq!(identity.ship_id, Some(20));
    assert_eq!(identity.station_id, Some(30));
    assert_eq!(identity.system_id, Some(40));
    assert_eq!(identity.corp_id, 50);
    gw.world.unregister_pilot_generation(character, Some(1));
    assert!(gw.world.online_session(character).is_some());
    gw.world.unregister_pilot_generation(character, Some(2));
    assert!(gw.world.online_session(character).is_none());
    gw.db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap();
    gw.db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}
