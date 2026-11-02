//! Real Lua RPC mail delivery through the public service dispatcher.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, external flock, and run alone (global Lua host)"]
async fn lua_mail_rpc_validates_and_delivers() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sender = fixture::create(&db).await.unwrap();
    let recipient = fixture::create(&db).await.unwrap();
    let dir = std::env::temp_dir().join(format!("infinity-lua-mail-{}-{}", std::process::id(), sender.character));
    if let Err(error) = std::fs::create_dir(&dir) {
        for f in [sender, recipient] { fixture::cleanup(&db, f).await.unwrap(); }
        panic!("could not create Lua script directory: {error}");
    }
    let test_dir = dir.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        std::fs::write(test_dir.join("mail.lua"), format!(r#"
            register.rpc('mailFixture', 'Send', function(ctx)
                assert(not pcall(dao.mail.send, ctx, {{{}}}, string.rep('x', 4096), 'body'))
                local id = dao.mail.send(ctx, {{{}}}, 'Lua subject', 'Lua body')
                return eve.int(id)
            end)
        "#, recipient.character, recipient.character)).unwrap();
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), sender.character).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(recipient.character, tx);
        let mut config = crate::config::Config::default();
        config.lua.enabled = true;
        config.lua.scripts_dir = test_dir.to_string_lossy().into_owned();
        crate::lua::start(&config, &test_db, &client.sde, &client.world).await.unwrap();
        let id = rpc(&mut client, "mailFixture", "Send", &[]).await.unwrap().as_int().unwrap();
        let headers = crate::dao::mail::headers(&test_db, recipient.character, &vec![id]).await.unwrap();
        assert_eq!(headers.len(), 1);
        let header = &headers[0];
        assert_eq!((header.message_id, header.sender_id, &header.to_character_ids, header.to_list_id, header.to_corp_or_alliance_id, &header.title), (id, sender.character, &recipient.character.to_string(), None, None, &"Lua subject".to_string()));
        assert_eq!(crate::dao::mail::body(&test_db, id, recipient.character).await.unwrap().as_deref(), Some("Lua body"));
        let received = crate::dao::mail::statuses(&test_db, recipient.character).await.unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!((received[0].message_id, received[0].status_mask, received[0].label_mask), (id, 0, 1));
        let sent = crate::dao::mail::statuses(&test_db, sender.character).await.unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].message_id, sent[0].status_mask, sent[0].label_mask), (id, 1, 2));
        assert_eq!(rx.try_recv().unwrap(), ("OnMailSent".to_string(), vec![Value::Int(id), Value::Int(sender.character), Value::Int(header.sent_date), batch(&[recipient.character]), Value::None, Value::None, Value::unicode("Lua subject"), Value::Int(0)]));
        assert!(rx.try_recv().is_err());
    }).await;
    let mail_cleanup = crate::dao::mailing_lists::tests::cleanup(&db, sender.character, &[sender.character, recipient.character]).await;
    let sender_cleanup = fixture::cleanup(&db, sender).await;
    let recipient_cleanup = fixture::cleanup(&db, recipient).await;
    let dir_cleanup = std::fs::remove_dir_all(dir);
    mail_cleanup.unwrap();
    sender_cleanup.unwrap();
    recipient_cleanup.unwrap();
    dir_cleanup.unwrap();
    result.unwrap();
}
