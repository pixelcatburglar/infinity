//! Durable chat authorization and manager RPC behavior.
use super::authority::{self, RoomConfig};
use crate::{dao::chat_channels as dao, db::Db, services::{Call, CallError}, session::ChatSession};
use marshal::Value;

fn session(id: i64) -> ChatSession {
    ChatSession { character_id: id, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }
}

async fn rpc(c: &mut crate::client::Client, method: &str, args: &[Value]) -> Result<Value, CallError> {
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service: "XmppChatMgr", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn durable_room_authority_and_private_identity() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let admin = crate::services::tests::fixture::create(&db).await.unwrap();
    let member = crate::services::tests::fixture::create(&db).await.unwrap();
    let outsider = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let display = format!("Chat fixture {}", owner.character);
        let player = rpc(&mut client, "CreatePlayerOwnedChannel", &[Value::unicode(&display)]).await.unwrap().as_text().unwrap().into_owned();
        assert!(matches!(rpc(&mut client, "CreatePlayerOwnedChannel", &[Value::unicode(display.to_uppercase())]).await, Err(CallError::User { .. })));
        let second_display = format!("Second chat fixture {}", owner.character);
        let second = rpc(&mut client, "CreatePlayerOwnedChannel", &[Value::unicode(second_display)]).await.unwrap().as_text().unwrap().into_owned();
        let first = rpc(&mut client, "CreatePrivateChannel", &[Value::Int(member.character)]).await.unwrap().as_text().unwrap().into_owned();
        client.session.apply(vec![("charid", Value::Int(member.character))]);
        let reverse = rpc(&mut client, "CreatePrivateChannel", &[Value::Int(owner.character)]).await.unwrap().as_text().unwrap().into_owned();
        assert_eq!(first, reverse);
        let private = dao::get(&test_db, &first).await.unwrap().unwrap();
        assert_eq!(private.owner_id, owner.character);
        assert_eq!(private.private_low, Some(owner.character.min(member.character)));
        assert_eq!(private.private_high, Some(owner.character.max(member.character)));
        let sde = &client.sde;
        let hub = &client.chat;
        let (o, a, m, x) = (session(owner.character), session(admin.character), session(member.character), session(outsider.character));
        assert!(authority::admit(&test_db, sde, &o, &first).await.unwrap().is_some());
        assert!(authority::admit(&test_db, sde, &m, &first).await.unwrap().is_some());
        assert!(authority::admit(&test_db, sde, &x, &first).await.unwrap().is_none());
        assert!(dao::members(&test_db, &first).await.unwrap().is_empty());
        authority::invite(&test_db, hub, &o, &first, member.character).await.unwrap();
        authority::invite(&test_db, hub, &m, &first, owner.character).await.unwrap();
        assert!(authority::invite(&test_db, hub, &x, &first, owner.character).await.is_err());
        assert!(authority::invite(&test_db, hub, &o, &first, outsider.character).await.is_err());
        assert!(authority::invite(&test_db, hub, &o, &first, owner.character).await.is_err());
        let private_after_invites = dao::get(&test_db, &first).await.unwrap().unwrap();
        assert!(!private_after_invites.public);
        assert_eq!(private_after_invites.private_low, Some(owner.character.min(member.character)));
        assert_eq!(private_after_invites.private_high, Some(owner.character.max(member.character)));
        assert!(dao::members(&test_db, &first).await.unwrap().is_empty());
        assert!(authority::admit(&test_db, sde, &o, &player).await.unwrap().is_some());
        assert!(authority::admit(&test_db, sde, &x, &player).await.unwrap().is_none());
        assert!(dao::set_member(&test_db, &player, owner.character, outsider.character, dao::Affiliation::None, true, dao::Voice::Voiced).await.unwrap());
        assert!(authority::admit(&test_db, sde, &x, &player).await.unwrap().is_none());
        assert!(!authority::disco_list(&test_db, &x).await.unwrap().iter().any(|r| r.name == player || r.name == first));
        assert!(dao::set_member(&test_db, &player, owner.character, outsider.character, dao::Affiliation::None, false, dao::Voice::Default).await.unwrap());
        assert!(authority::invite(&test_db, hub, &x, &player, member.character).await.is_err());
        authority::invite(&test_db, hub, &o, &player, member.character).await.unwrap();
        let invited = dao::member(&test_db, &player, member.character).await.unwrap().unwrap();
        assert_eq!(invited.affiliation, dao::Affiliation::Member);
        assert!(invited.invited);
        assert!(authority::admit(&test_db, sde, &m, &player).await.unwrap().is_some());
        assert!(authority::set_affiliation(&test_db, hub, &m, &player, admin.character, "admin").await.is_err());
        authority::set_affiliation(&test_db, hub, &o, &player, admin.character, "admin").await.unwrap();
        assert!(authority::set_affiliation(&test_db, hub, &a, &player, outsider.character, "admin").await.is_err());
        assert!(authority::set_affiliation(&test_db, hub, &a, &player, owner.character, "outcast").await.is_err());
        assert!(authority::set_affiliation(&test_db, hub, &a, &player, member.character, "owner").await.is_err());
        assert!(authority::set_subject(&test_db, hub, &m, &player, "forbidden").await.is_err());
        authority::set_subject(&test_db, hub, &a, &player, "<durable & subject>").await.unwrap();
        let config = RoomConfig { display_name: display.clone(), public: true, moderated: true, members_by_default: false };
        assert!(authority::configure(&test_db, hub, &m, &player, config.clone()).await.is_err());
        authority::configure(&test_db, hub, &a, &player, config.clone()).await.unwrap();
        let conflicting = RoomConfig { display_name: display.to_uppercase(), public: true, moderated: false, members_by_default: true };
        assert!(matches!(authority::configure(&test_db, hub, &o, &second, conflicting).await, Err(CallError::User { .. })));
        assert!(!dao::get(&test_db, &second).await.unwrap().unwrap().public);
        let guest = authority::admit(&test_db, sde, &x, &player).await.unwrap().unwrap();
        assert_eq!(guest.affiliation, "none");
        assert_eq!(guest.role, "visitor");
        assert_eq!(guest.motd, "<durable & subject>");
        authority::set_role(&test_db, hub, &a, &player, outsider.character, "visitor").await.unwrap();
        assert_eq!(authority::admit(&test_db, sde, &x, &player).await.unwrap().unwrap().role, "visitor");
        authority::set_role(&test_db, hub, &a, &player, outsider.character, "participant").await.unwrap();
        assert_eq!(authority::admit(&test_db, sde, &x, &player).await.unwrap().unwrap().role, "participant");
        let closed = RoomConfig { public: false, ..config.clone() };
        authority::configure(&test_db, hub, &o, &player, closed).await.unwrap();
        assert!(authority::admit(&test_db, sde, &x, &player).await.unwrap().is_none());
        assert_eq!(dao::member(&test_db, &player, outsider.character).await.unwrap().unwrap().voice, dao::Voice::Voiced);
        authority::configure(&test_db, hub, &o, &player, config).await.unwrap();
        let restarted = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let reloaded = authority::admit(&restarted, sde, &x, &player).await.unwrap().unwrap();
        assert_eq!(reloaded.role, "participant");
        assert_eq!(reloaded.motd, "<durable & subject>");
        authority::set_affiliation(&test_db, hub, &a, &player, outsider.character, "outcast").await.unwrap();
        assert!(authority::admit(&test_db, sde, &x, &player).await.unwrap().is_none());
        assert!(authority::invite(&test_db, hub, &a, &player, outsider.character).await.is_err());
        assert!(authority::destroy(&test_db, hub, &m, &player).await.is_err());
        assert!(dao::get(&test_db, &player).await.unwrap().is_some());
        authority::destroy(&test_db, hub, &a, &player).await.unwrap();
        assert!(dao::get(&test_db, &player).await.unwrap().is_none());
        assert!(dao::members(&test_db, &player).await.unwrap().is_empty());
    }).await;
    db.execute("DELETE FROM game.chat_channels WHERE owner_id = ANY($1)", &[&vec![owner.character, admin.character, member.character, outsider.character]]).await.unwrap();
    for f in [owner, admin, member, outsider] { crate::services::tests::fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[test]
fn invalid_stored_chat_enums_are_errors() {
    use tokio_postgres::types::{FromSql, Type};
    assert!(dao::Voice::from_sql(&Type::TEXT, b"bogus").is_err());
    assert!(dao::Affiliation::from_sql(&Type::TEXT, b"bogus").is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn long_name_and_owner_transfer_authority() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let member = crate::services::tests::fixture::create(&db).await.unwrap();
    let outsider = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (client, _peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let hub = &client.chat;
        let (o, m, x) = (session(owner.character), session(member.character), session(outsider.character));
        let display = "界".repeat(60);
        let name = super::authority::create_player(&test_db, owner.character, &display).await.unwrap();
        let config = RoomConfig { display_name: display.clone(), public: true, moderated: false, members_by_default: false };
        authority::configure(&test_db, hub, &o, &name, config.clone()).await.unwrap();
        let stored = dao::get(&test_db, &name).await.unwrap().unwrap();
        assert_eq!(stored.display_name, display);
        assert!(stored.public);
        assert!(authority::configure(&test_db, hub, &o, &name, RoomConfig { display_name: "界".repeat(51), public: false, ..config.clone() }).await.is_err());
        let stored = dao::get(&test_db, &name).await.unwrap().unwrap();
        assert_eq!(stored.display_name, display);
        assert!(stored.public);
        authority::configure(&test_db, hub, &o, &name, RoomConfig { display_name: "界".repeat(50), ..config }).await.unwrap();
        assert_eq!(dao::get(&test_db, &name).await.unwrap().unwrap().display_name, "界".repeat(50));
        assert!(authority::invite(&test_db, hub, &x, &name, member.character).await.is_err());
        authority::set_affiliation(&test_db, hub, &o, &name, member.character, "owner").await.unwrap();
        assert_eq!(dao::get(&test_db, &name).await.unwrap().unwrap().owner_id, member.character);
        assert!(authority::set_affiliation(&test_db, hub, &o, &name, outsider.character, "owner").await.is_err());
        authority::set_affiliation(&test_db, hub, &m, &name, outsider.character, "owner").await.unwrap();
        let private = super::authority::create_private(&test_db, owner.character, member.character).await.unwrap();
        assert!(authority::invite(&test_db, hub, &o, &private, outsider.character).await.is_err());
        assert!(authority::set_affiliation(&test_db, hub, &o, &private, outsider.character, "owner").await.is_err());
        assert!(authority::configure(&test_db, hub, &o, &private, RoomConfig { display_name: "private".into(), public: true, moderated: false, members_by_default: false }).await.is_err());
    }).await;
    db.execute("DELETE FROM game.chat_channels WHERE owner_id = ANY($1)", &[&vec![owner.character, member.character, outsider.character]]).await.unwrap();
    for f in [owner, member, outsider] { crate::services::tests::fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
