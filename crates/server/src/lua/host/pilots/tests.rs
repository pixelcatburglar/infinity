//! Lua-facing simulated pilot lifecycle and cancellation coverage.
mod economy;
mod skills;
mod effects;
mod fixture;
mod space;
use super::*;

#[tokio::test]
async fn cancelled_lua_waiter_does_not_cancel_owned_mutation() {
    let lua = Lua::new();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let inputs = Arc::new(Mutex::new(Some((entered_tx, release_rx, done_tx))));
    lua.globals().set("operation", lua.create_async_function(move |_, ()| {
        let (entered, release, done) = inputs.lock().unwrap().take().unwrap();
        async move {
            mutate(async move {
                let _ = entered.send(());
                let _ = release.await;
                let _ = done.send(());
                Ok(())
            }).await.map_err(lua_error)
        }
    }).unwrap()).unwrap();
    let mut call = Box::pin(lua.load("operation()").exec_async());
    tokio::select! { result = &mut call => panic!("finished before gate: {result:?}"), result = entered_rx => result.unwrap() }
    drop(call);
    release_tx.send(()).unwrap();
    done_rx.await.unwrap();
}

#[tokio::test]
async fn panicking_owned_mutation_returns_internal_error() {
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), mutate(
        std::future::poll_fn(|_| -> std::task::Poll<Result<(), CallError>> {
            panic!("owned mutation panic")
        }),
    )).await.expect("panicking mutation must complete its caller");
    assert!(matches!(result, Err(CallError::Internal(_))));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn lua_lifecycle_authorizes_and_survives_reload() {
    let r = fixture::resources().await;
    let (race, bloodline, school, station) = fixture::creation_inputs(&r).await;
    let lua = Lua::new();
    register(&lua, &lua.globals(), &r).unwrap();
    let globals = lua.globals();
    globals.set("race", race).unwrap();
    globals.set("bloodline", bloodline).unwrap();
    globals.set("school", school).unwrap();
    globals.set("station", station).unwrap();
    let name = format!("LuaPilot{}", crate::space::now_ms());
    globals.set("pilot_name", name).unwrap();
    for source in [
        "return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0,typo=true}",
        "return pilots.create{name=pilot_name,race_id=2147483648,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}",
        "return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=2,balance=0}",
        "return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=-1}",
    ] {
        assert!(lua.load(source).eval_async::<Value>().await.is_err());
    }
    let id: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("id", id).unwrap();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let before: Table = lua.load("return pilots.identity(id)").eval_async().await?;
        assert_eq!(before.get::<i64>("station_id")?, station);
        assert_eq!(before.get::<Option<i64>>("ship_id")?, None);
        assert!(!before.get::<bool>("online")?);
        r.db.execute("UPDATE game.characters SET station_id=NULL WHERE character_id=$1", &[&id]).await?;
        assert!(lua.load("return pilots.online(id)").eval_async::<bool>().await.is_err());
        assert!(!r.world.is_online(id));
        assert!(r.world.chat_session(id).is_none());
        assert!(r.pilots.state(id).lock().await.is_none());
        r.db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&id, &station]).await?;
        assert!(lua.load("return pilots.online(id)").eval_async::<bool>().await?);
        assert!(!lua.load("return pilots.online(id)").eval_async::<bool>().await?);
        let active: Table = lua.load("return pilots.identity(id)").eval_async().await?;
        let ship = active.get::<i64>("ship_id")?;
        assert!(active.get::<bool>("online")?);
        assert!(r.world.is_online(id));
        assert!(r.world.chat_session(id).is_some());
        let fitted: i64 = r.db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id <> $3", &[&id, &ship, &crate::consts::FLAG_CARGO]).await?.get("n");
        assert!(fitted > 0, "rookie ship must have its normal initial fit");
        for _ in 0..100 { r.world.notify_pilot(id, "OnTest", vec![]); }
        let dir = std::env::temp_dir().join(format!("lua-pilot-reload-{}-{id}", std::process::id()));
        std::fs::create_dir(&dir)?;
        let script = dir.join("pilot.lua");
        std::fs::write(&script, "register.rpc('pilot','online',function(id) return pilots.online(id) end); register.rpc('pilot','identity',function(id) return pilots.identity(id) end)")?;
        let config = crate::config::lua::LuaConfig { scripts_dir: dir.to_string_lossy().into_owned(), ..Default::default() };
        let host = crate::lua::reload::Host::new(config, r.clone()).await?;
        host.reload().await?;
        let generation = host.snapshot();
        let online = &generation.claims.rpc[&("pilot".into(), "online".into())].function;
        assert!(!online.call_async::<bool>(id).await?);
        let identity = &generation.claims.rpc[&("pilot".into(), "identity".into())].function;
        let reloaded: Table = identity.call_async(id).await?;
        assert_eq!(reloaded.get::<i64>("ship_id")?, ship);
        assert!(reloaded.get::<bool>("online")?);
        std::fs::write(&script, "invalid !")?;
        assert!(host.reload().await.is_err());
        assert!(r.world.is_online(id));
        std::fs::remove_dir_all(dir)?;
        lua.load("pilots.offline(id); pilots.offline(id)").exec_async().await?;
        assert!(!r.world.is_online(id));
        assert!(r.world.chat_session(id).is_none());
        let after: Table = lua.load("return pilots.identity(id)").eval_async().await?;
        assert_eq!(after.get::<i64>("ship_id")?, ship);
        assert!(!after.get::<bool>("online")?);
        for source in ["return pilots.identity(0)", "return pilots.online(-1)", "return pilots.offline(0)", "return pilots.identity(9223372036854775807)"] {
            assert!(lua.load(source).eval_async::<Value>().await.is_err());
        }
        r.db.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&id]).await?;
        for source in ["return pilots.identity(id)", "return pilots.online(id)", "return pilots.offline(id)"] {
            assert!(lua.load(source).eval_async::<Value>().await.is_err());
        }
        r.db.execute("UPDATE game.characters SET deleted=false WHERE character_id=$1", &[&id]).await?;
        let foreign: i64 = r.db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('pilot-foreign-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"test"[..]]).await?.get("user_id");
        r.db.execute("UPDATE game.characters SET user_id=$2 WHERE character_id=$1", &[&id, &foreign]).await?;
        for source in ["return pilots.identity(id)", "return pilots.online(id)", "return pilots.offline(id)"] {
            assert!(lua.load(source).eval_async::<Value>().await.is_err());
        }
        r.db.execute("UPDATE game.characters SET user_id=$2 WHERE character_id=$1", &[&id, &r.pilots.account]).await?;
        r.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&foreign]).await?;
        Ok(())
    }.await;
    if r.world.is_online(id) { let _ = lifecycle::offline(r.clone(), id).await; }
    fixture::cleanup(&r.db, id).await;
    result.unwrap();
}
