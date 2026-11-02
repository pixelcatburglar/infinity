//! Real database coverage for the Lua script roots shown by the list command.

use super::super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn list_shows_script_roots() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let dir = std::env::temp_dir().join(format!("infinity-lua-roots-list-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let task_dir = dir.clone();
    let result = tokio::spawn(async move {
        let primary = task_dir.join("primary");
        let extra = task_dir.join("extra");
        std::fs::create_dir(&primary).unwrap();
        std::fs::create_dir(&extra).unwrap();
        std::fs::write(primary.join("primary.lua"), "register.tick{name='heartbeat', interval_ms=100, fn=function(now) end}").unwrap();
        std::fs::write(extra.join("extra.lua"), "").unwrap();
        let world = World::new(db.clone(), sde.clone(), 1);
        let mut config = Config::default();
        config.lua.scripts_dir = primary.to_string_lossy().into_owned();
        config.lua.mod_dirs = vec![extra.to_string_lossy().into_owned()];
        start(&config, &db, &sde, &world).await.unwrap();
        let cfg = Arc::new(config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: 1, user_name: "roots".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, chat, world, login);
        let list = command(&mut client, "lua", &["list".into()]).await.unwrap().unwrap();
        assert!(list.contains(&format!("script: primary root={}", primary.canonicalize().unwrap().display())));
        assert!(list.contains(&format!("script: extra root={}", extra.canonicalize().unwrap().display())));
        assert!(list.contains("primary: tick heartbeat interval_ms=100 skipped=0"));
        drop(peer);
    }).await;
    std::fs::remove_dir_all(&dir).unwrap();
    result.unwrap();
}
