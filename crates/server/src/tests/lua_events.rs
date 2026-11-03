//! Real-host observer for pilot event producer acceptance tests.
//! Events are recorded in the host's Lua state; the sentinel is only a drain barrier.

mod target_warp;
mod damage;
mod cycles;

use std::{future::Future, path::PathBuf, sync::{Arc, atomic::{AtomicI64, Ordering}}, time::Duration};
use crate::lua::{self, events::GameEvent, host::{self, Resources}};
use crate::{client::Client, services::{self, Call}};
use marshal::Value;
use tokio::sync::Mutex;

const OBSERVER: &str = r#"
local events = {}
local barrier = 0
local function record(ev)
    events[#events + 1] = ev
end
register.event('Targeted', record)
register.event('Damaged', record)
register.event('ModuleCycleEnded', record)
register.event('WarpArrived', record)
register.event('CharacterSelected', function(ev)
    barrier = ev.user_id
end)
register.rpc('lua_pilot_event_observer', 'snapshot', function(ctx, args, kwargs)
    local result = {}
    for i, ev in ipairs(events) do
        local fields = {}
        for key, value in pairs(ev) do
            if type(value) == 'number' then
                fields[key] = eve.int(value)
            elseif type(value) == 'string' then
                fields[key] = eve.str(value)
            else
                error('unexpected event field type')
            end
        end
        result[i] = eve.dict(fields)
    end
    return eve.dict({barrier = eve.int(barrier), events = eve.list(result)})
end)
"#;

pub(super) struct Context {
    pub resources: Resources,
    pub lua: mlua::Lua,
    client: Mutex<Client>,
    _peer: tokio::net::TcpStream,
    driver: String,
    dir: PathBuf,
    next_barrier: AtomicI64,
}

impl Context {
    pub(super) async fn new() -> Self {
        crate::tests::init_config();
        let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
        let world = crate::space::World::new(db.clone(), sde.clone(), 1);
        let mut config = crate::config::Config::default();
        let unique = format!("{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let observer = format!("event_observer_{unique}");
        let driver = format!("event_driver_{unique}");
        let dir = std::env::temp_dir().join(format!("infinity-lua-events-{unique}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join(format!("{observer}.lua")), OBSERVER).unwrap();
        config.lua.scripts_dir = dir.to_string_lossy().into_owned();
        let hub = crate::chat::Hub::new(&config.chat.domain, db.clone(), world.clone());
        let pilots = host::pilots::Pilots::new(&db, hub.clone()).await.unwrap();
        let resources = Resources { db: db.clone(), sde: sde.clone(), world: world.clone(), pilots, store_value_limit_bytes: config.lua.store_value_limit_bytes };
        lua::start(&config, &db, &sde, &world, &hub).await.unwrap();
        let lua = mlua::Lua::new();
        lua::value::register(&lua, &lua.globals()).unwrap();
        host::register(&lua, &lua.globals(), &driver, &toml::Table::new(), &resources).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: 0, user_name: "event-observer".into(), role: 0, client_id: 0, session_id: 0, language_id: "EN".into() };
        let client = Client::new(crate::conn::Conn::new(stream, address), Arc::new(config), db, sde, hub, world, login);
        Self { resources, lua, client: Mutex::new(client), _peer: peer, driver, dir, next_barrier: AtomicI64::new(0) }
    }

    async fn snapshot_after_barrier(&self) -> Vec<serde_json::Value> {
        let mut client = self.client.lock().await;
        let n = self.next_barrier.fetch_add(1, Ordering::Relaxed) + 1;
        lua::on_event(GameEvent::CharacterSelected { user_id: n, char_id: 0 });
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let kwargs = Value::Dict(vec![]);
                let call = Call { service: "lua_pilot_event_observer", method: "snapshot", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
                let snapshot = services::dispatch(&mut client, &call).await.expect("event observer snapshot failed");
                if services::value_field(&snapshot, "barrier").and_then(Value::as_int) == Some(n) {
                    let Some(Value::List(events)) = services::value_field(&snapshot, "events") else {
                        panic!("event observer snapshot missing events list");
                    };
                    return events.iter().map(|event| {
                        let Value::Dict(fields) = event else { panic!("event observer record is not a dictionary"); };
                        let mut object = serde_json::Map::new();
                        for (key, value) in fields {
                            let key = key.as_text().expect("event field name is not text").into_owned();
                            let value = match value {
                                Value::Int(value) => serde_json::json!(value),
                                Value::Bytes(_) | Value::Unicode(_) => serde_json::json!(value.as_text().unwrap()),
                                _ => panic!("unexpected event field value"),
                            };
                            assert!(object.insert(key, value).is_none(), "duplicate event field");
                        }
                        serde_json::Value::Object(object)
                    }).collect();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("event observer did not reach barrier")
    }

    pub(super) async fn drain(&self) {
        self.snapshot_after_barrier().await;
    }

    pub(super) async fn events(&self) -> Vec<serde_json::Value> {
        self.snapshot_after_barrier().await
    }

    pub(super) async fn cleanup(&self) {
        for key in crate::dao::lua_store::keys(&self.resources.db, &self.driver, "").await.unwrap() {
            crate::dao::lua_store::delete(&self.resources.db, &self.driver, &key).await.unwrap();
        }
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn real_pilot_event_producers() {
    tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).with_test_writer().init();
    let ctx = Context::new().await;
    let mut phases = std::pin::pin!(async {
        target_warp::run(&ctx).await;
        damage::run(&ctx).await;
        cycles::run(&ctx).await;
    });
    let result = std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| phases.as_mut().poll(cx))) {
        Ok(result) => result.map(Ok),
        Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await;
    ctx.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
