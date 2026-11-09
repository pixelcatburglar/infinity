use super::*;

#[test]
fn validates_contact_arguments_and_batches() {
    let good = [Value::Int(123), Value::Float(-10.0), Value::Bool(true), Value::Int(1), Value::unicode("hello")];
    let parsed = parse_contact(&good, false).ok().unwrap();
    assert_eq!((parsed.id, parsed.relationship, parsed.watchlist, parsed.notify), (123, -10.0, true, true));
    assert!(parse_contact(&[Value::Int(123)], false).unwrap_or_else(|_| panic!("agent form")).agent);
    assert!(parse_contact(&[Value::Int(123)], true).is_err());
    for bad in [Value::Float(f64::NAN), Value::Float(f64::INFINITY), Value::Float(10.01), Value::Bool(true)] {
        assert!(relationship(&bad).is_err());
    }
    assert!(id(&Value::Bool(true)).is_err());
    assert!(id(&Value::Int(0)).is_err());
    assert!(ids(&Value::list([Value::Int(1), Value::unicode("bad")])).is_err());
    assert!(boolean(&Value::Int(2)).is_err());
}

#[test]
fn accepts_none_contact_message_and_rejects_nontext() {
    let mut args = [Value::Int(123), Value::Float(0.0), Value::Bool(false), Value::Bool(false), Value::None];
    assert_eq!(parse_contact(&args, false).ok().unwrap().message, "");
    assert_eq!(parse_contact(&args, true).ok().unwrap().message, "");
    args[4] = Value::Int(1);
    assert!(parse_contact(&args, false).is_err());
    assert!(parse_contact(&args, true).is_err());
}

#[test]
fn notification_payload_preserves_text() {
    let message = "引号\" slash\\ newline\n";
    let data: serde_json::Value = serde_json::from_str(&payload(-5.0, message)).unwrap();
    assert_eq!(data["message"], message);
    assert_eq!(data["level"], -5.0);
}

#[test]
fn contact_list_has_client_columns_and_values() {
    let result = contact_list(vec![dao::Contact { contact_id: 42, relationship: -2.5, in_watchlist: true, label_mask: 7 }], vec![43]);
    let Value::Instance(_, state) = result else { panic!("expected KeyVal"); };
    let Value::Instance(_, addresses) = state.get("addresses").unwrap() else { panic!("expected rowset"); };
    assert_eq!(addresses.get("header"), Some(&Value::list([Value::str("contactID"), Value::str("inWatchlist"), Value::str("relationshipID"), Value::str("labelMask")])));
    assert_eq!(addresses.get("lines"), Some(&Value::list([Value::list([Value::Int(42), Value::Bool(true), Value::Float(-2.5), Value::Int(7)])])));
    let Value::Instance(_, blocked) = state.get("blocked").unwrap() else { panic!("expected rowset"); };
    assert_eq!(blocked.get("header"), Some(&Value::list([Value::str("senderID")])));
    assert_eq!(blocked.get("lines"), Some(&Value::list([Value::list([Value::Int(43)])])));
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL"]
async fn persistence_independent_blocks_and_transaction_rollback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let me = -crate::client::filetime_now();
    let target = me + 1;
    let other = me + 2;
    let dispatch_called = AtomicBool::new(false);
    let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        dao::upsert(tx, me, target, 4.0, true).await.map_err(db_err)?;
        mail::notify(tx, outbox, target, CONTACT_ADD, CONTACT_GROUP, me, &payload(4.0, "\"\n中文")).await?;
        Err(invalid())
    }), |(), _| async { dispatch_called.store(true, Ordering::SeqCst); Ok(()) }).await;
    assert!(failed.is_err());
    assert!(!dispatch_called.load(Ordering::SeqCst));
    assert!(dao::list(&db, me).await.unwrap().is_empty());
    assert!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND sender_id=$2", &[&target, &me]).await.unwrap().is_empty());
    let committed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        dao::upsert(tx, me, target, 4.0, true).await.map_err(db_err)?;
        mail::notify(tx, outbox, target, CONTACT_ADD, CONTACT_GROUP, me, &payload(4.0, "\"\n中文")).await?;
        Ok(())
    }), |(), outbox| async move { assert_eq!(outbox.into_entries().len(), 1); Ok(()) }).await;
    assert!(committed.is_ok());
    let rows = dao::list(&db, me).await.unwrap();
    assert_eq!((rows[0].contact_id, rows[0].relationship, rows[0].in_watchlist), (target, 4.0, true));
    let notices = db.query("SELECT data FROM game.notifications WHERE receiver_id=$1 AND sender_id=$2", &[&target, &me]).await.unwrap();
    let data: String = notices[0].get("data");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&data).unwrap()["message"], "\"\n中文");
    dao::upsert(&db, me, target, -3.0, false).await.unwrap();
    let rows = dao::set_blocked(&db, me, &[target, other], true).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].relationship, -3.0);
    assert_eq!(dao::blocked(&db, me).await.unwrap().len(), 2);
    dao::delete(&db, me, &[target]).await.unwrap();
    assert_eq!(dao::blocked(&db, me).await.unwrap().len(), 2);
    dao::upsert(&db, me, target, 1.0, true).await.unwrap();
    dao::set_blocked(&db, me, &[target, other], false).await.unwrap();
    assert_eq!(dao::list(&db, me).await.unwrap().len(), 1);
    assert!(dao::blocked(&db, me).await.unwrap().is_empty());
    dao::delete(&db, me, &[target]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=$1 AND sender_id=$2", &[&target, &me]).await.unwrap();
}

async fn char_call(client: &mut Client, method: &str, args: &[Value]) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let call = crate::services::Call { service: "charMgr", method, args, kwargs: &kwargs, bound: false, bind_params: None };
    super::super::char_mgr::dispatch(client, &call).await.expect("charMgr method")
}

fn initial_rows(value: Value) -> Vec<(i64, bool)> {
    let Value::Reduce { data, .. } = value else { panic!("expected CRowset") };
    let parts = data.items().unwrap();
    let args = parts[1].items().unwrap();
    let descriptor = marshal::DbRowDescriptor::from_value(&args[0]).unwrap();
    args[1].items().unwrap().iter().map(|row| {
        let Some(Value::Int(id)) = descriptor.get(row, "contactID") else { panic!("contact ID") };
        let Some(Value::Bool(online)) = descriptor.get(row, "online") else { panic!("online status") };
        (id, online)
    }).collect()
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn service_contacts_and_presence() {
    use crate::{conn::Conn, handshake::Login, space::World, services::online_status};
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let account = -now;
    let owner = 1_500_000_000 + now % 100_000_000;
    let target = owner + 1;
    let other = owner + 2;
    let nonwatcher = owner + 3;
    let name = format!("contacts-test-{now}");
    let password: Vec<u8> = Vec::new();
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,$2,$3,0)", &[&account, &name, &password]).await.unwrap();
    for (id, suffix) in [(owner, "owner"), (target, "target"), (other, "other"), (nonwatcher, "nonwatcher")] {
        let name = format!("{name}-{suffix}");
        db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,0)", &[&id, &account, &name, &account]).await.unwrap();
    }
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let agent = sde.characters.npc_characters().find(|npc| crate::services::missions::agent(npc.character_id).is_some()).expect("retail agent in SDE").character_id;
    let nonagent = sde.characters.npc_characters().find(|npc| crate::services::missions::agent(npc.character_id).is_none()).expect("non-agent NPC in SDE").character_id;
    let world = World::new(db.clone(), sde.clone(), 0);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let mut client = Client::new(Conn::new(stream, address), Arc::new(crate::config::Config::default()), db.clone(), sde,
        crate::chat::Hub::new("localhost", db.clone(), world.clone()), world.clone(),
        Login { user_id: account, user_name: name, role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
    client.session.apply(vec![("charid", Value::Int(owner))]);
    let (owner_tx, mut owner_rx) = tokio::sync::mpsc::unbounded_channel();
    let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
    let (nonwatcher_tx, mut nonwatcher_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(owner, owner_tx);
    world.register_pilot(nonwatcher, nonwatcher_tx);
    assert!(char_call(&mut client, "AddContact", &[Value::Int(agent)]).await.is_ok());
    assert_eq!(owner_rx.try_recv().unwrap().0, "OnPersonalContactsUpdated");
    assert!(char_call(&mut client, "AddContact", &[Value::Int(nonagent)]).await.is_err());
    assert!(owner_rx.try_recv().is_err());
    assert!(char_call(&mut client, "AddContact", &[Value::Int(target), Value::Float(5.0), Value::Bool(true), Value::Bool(true), Value::unicode("hello\n中文")]).await.is_ok());
    assert_eq!(owner_rx.try_recv().unwrap().0, "OnPersonalContactsUpdated");
    let row = db.query_one("SELECT type_id,data FROM game.notifications WHERE receiver_id=$1 AND sender_id=$2", &[&target, &owner]).await.unwrap();
    assert_eq!(row.get::<_, i32>("type_id"), CONTACT_ADD);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&row.get::<_, String>("data")).unwrap()["message"], "hello\n中文");
    let list = char_call(&mut client, "GetContactList", &[]).await.ok().unwrap();
    let Value::Instance(_, state) = list else { panic!("expected KeyVal") };
    let Value::Instance(_, addresses) = state.get("addresses").unwrap() else { panic!("expected address rowset") };
    let lines = addresses.get("lines").unwrap().items().unwrap();
    assert!(lines.iter().any(|line| line.items().unwrap()[0] == Value::Int(agent)));
    assert!(lines.iter().any(|line| line.items().unwrap()[0] == Value::Int(target)));
    assert!(!lines.iter().any(|line| line.items().unwrap()[0] == Value::Int(nonagent)));

    client.session.apply(vec![("charid", Value::Int(nonwatcher))]);
    assert!(char_call(&mut client, "AddContact", &[Value::Int(target), Value::Float(0.0), Value::Bool(false), Value::Bool(false), Value::None]).await.is_ok());
    assert_eq!(nonwatcher_rx.try_recv().unwrap().0, "OnPersonalContactsUpdated");
    let kwargs = Value::Dict(vec![]);
    let call = crate::services::Call { service: "onlineStatus", method: "GetInitialState", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(initial_rows(online_status::dispatch(&client, &call).await.unwrap().ok().unwrap()), vec![(target, false)]);
    client.session.apply(vec![("charid", Value::Int(owner))]);
    let offline = initial_rows(online_status::dispatch(&client, &call).await.unwrap().ok().unwrap());
    assert!(offline.contains(&(target, false)));
    assert!(offline.contains(&(agent, false)));
    world.register_pilot(target, target_tx);
    let online = initial_rows(online_status::dispatch(&client, &call).await.unwrap().ok().unwrap());
    assert!(online.contains(&(target, true)));
    assert!(online.contains(&(agent, false)));
    client.session.apply(vec![("charid", Value::Int(nonwatcher))]);
    assert_eq!(initial_rows(online_status::dispatch(&client, &call).await.unwrap().ok().unwrap()), vec![(target, true)]);
    client.session.apply(vec![("charid", Value::Int(owner))]);
    online_status::presence_changed(&db, &world, target, true).await;
    let event = owner_rx.try_recv().unwrap();
    assert_eq!((event.0.as_str(), event.1), ("OnContactLoggedOn", vec![Value::Int(target)]));
    assert!(nonwatcher_rx.try_recv().is_err());

    assert!(char_call(&mut client, "EditContact", &[Value::Int(target), Value::Float(-2.0), Value::Bool(true), Value::Bool(true), Value::unicode("edited")]).await.is_ok());
    assert_eq!(target_rx.try_recv().unwrap().0, "OnNotificationReceived");
    assert_eq!(owner_rx.try_recv().unwrap().0, "OnPersonalContactsUpdated");
    world.unregister_pilot_generation(target, None);
    assert!(!world.is_online(target));
    online_status::presence_changed(&db, &world, target, false).await;
    let event = owner_rx.try_recv().unwrap();
    assert_eq!((event.0.as_str(), event.1), ("OnContactLoggedOff", vec![Value::Int(target)]));
    assert!(nonwatcher_rx.try_recv().is_err());
    assert!(char_call(&mut client, "EditContactsRelationshipID", &[Value::list([Value::Int(target)]), Value::Float(3.0)]).await.is_ok());
    let update = owner_rx.try_recv().unwrap();
    assert_eq!(update.1, vec![Value::list([Value::Int(target)]), Value::Float(3.0), Value::None]);
    assert!(char_call(&mut client, "BlockOwners", &[Value::list([Value::Int(target), Value::Int(other)])]).await.is_ok());
    let update = owner_rx.try_recv().unwrap();
    assert_eq!(update.1[0], Value::list([Value::Int(target)]));
    assert!(owner_rx.try_recv().is_err());
    assert!(char_call(&mut client, "DeleteContacts", &[Value::list([Value::Int(target)])]).await.is_ok());
    assert_eq!(owner_rx.try_recv().unwrap().0, "OnPersonalContactsUpdated");
    assert_eq!(dao::blocked(&db, owner).await.unwrap().len(), 2);
    assert!(char_call(&mut client, "AddContact", &[Value::Int(target), Value::Float(1.0), Value::Bool(false), Value::Bool(false), Value::None]).await.is_ok());
    assert_eq!(owner_rx.try_recv().unwrap().0, "OnPersonalContactsUpdated");
    assert!(char_call(&mut client, "UnblockOwners", &[Value::list([Value::Int(target), Value::Int(other)])]).await.is_ok());
    assert!(dao::blocked(&db, owner).await.unwrap().is_empty());
    assert!(dao::list(&db, owner).await.unwrap().iter().any(|row| row.contact_id == target));
    let notices = db.query("SELECT type_id FROM game.notifications WHERE receiver_id=$1 AND sender_id=$2 ORDER BY notification_id", &[&target, &owner]).await.unwrap();
    assert_eq!(notices.iter().map(|r| r.get::<_, i32>("type_id")).collect::<Vec<_>>(), vec![CONTACT_ADD, CONTACT_EDIT]);
    dao::delete(&db, owner, &[agent, target]).await.unwrap();
    dao::delete(&db, nonwatcher, &[target]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=$1 AND sender_id=$2", &[&target, &owner]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
