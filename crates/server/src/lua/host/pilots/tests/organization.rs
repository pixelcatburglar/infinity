//! Real Lua headless session changes through the shared corporation outbox.
use super::*;
use crate::{db::outbox::Outbox, services::corps::exit::Admission};
use std::time::Duration;

/// Test fixture clearance between the ship and station collision surfaces (metres).
const DOCK_FIXTURE_CLEARANCE: f64 = 100.0;

async fn place_stationary_at_station(r: &Resources, id: i64, ship: i64, system: i64, station: i64) -> anyhow::Result<()> {
    let (sent, received) = tokio::sync::oneshot::channel();
    r.world.with(system, move |s| {
        let station_ball = s.ball(station).expect("station ball");
        let position = station_ball.position_at(s.now_ms());
        let station_radius = f64::from(station_ball.radius);
        s.command(ship, move |ball, _, _| {
            ball.pos = [position[0] + station_radius + f64::from(ball.radius) + DOCK_FIXTURE_CLEARANCE, position[1], position[2]];
            ball.vel = [0.0; 3];
            ball.stop();
            let _ = sent.send(());
            Vec::new()
        });
    }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped"))?;
    tokio::time::timeout(Duration::from_secs(10), received).await??;
    let gap = r.world.with(system, move |s| {
        let ball = s.pilot_ball(id).filter(|ball| ball.id == ship)?;
        let station_ball = s.ball(station)?;
        let gap = crate::space::dist(ball.position_at(s.now_ms()), station_ball.position_at(s.now_ms()))
            - f64::from(ball.radius) - f64::from(station_ball.radius);
        Some((gap, ball.mode, ball.vel))
    }).await.flatten().ok_or_else(|| anyhow::anyhow!("pilot or station missing from actor"))?;
    assert!(gap.0 > 0.0 && gap.0 < 2.0 * DOCK_FIXTURE_CLEARANCE, "stationary fixture must have a small positive surface gap");
    assert_eq!((gap.1, gap.2), (crate::space::MODE_STOP, [0.0; 3]));
    let session = r.world.online_session(id).ok_or_else(|| anyhow::anyhow!("pilot not online"))?;
    assert_eq!((session.ship_id, session.system_id, session.station_id), (Some(ship), Some(system), None));
    let current = crate::dao::characters::ship::current(&r.db, id).await?.ok_or_else(|| anyhow::anyhow!("pilot missing"))?;
    assert_eq!((current.ship, current.system, current.station), (Some(ship), Some(system), None));
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn headless_undock_and_canonical_kick_reconcile_sessions() {
    let r = fixture::resources().await;
    let (race, bloodline, school, station) = fixture::creation_inputs(&r).await;
    let fields = fixture::FieldSnapshot::capture(&r, station).await;
    let system = r.sde.map.station(station).unwrap().solar_system_id.unwrap();
    let lua = Lua::new();
    register(&lua, &lua.globals(), &r).unwrap();
    let globals = lua.globals();
    globals.set("race", race).unwrap(); globals.set("bloodline", bloodline).unwrap();
    globals.set("school", school).unwrap(); globals.set("station", station).unwrap();
    globals.set("pilot_name", format!("HeadlessMember{}", crate::space::now_ms())).unwrap();
    let id: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("pilot_name", format!("HeadlessDirector{}", crate::space::now_ms())).unwrap();
    let ceo: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    let marker: i64 = r.db.query_one("SELECT COALESCE(MAX(notification_id),0) AS marker FROM game.notifications WHERE receiver_id=ANY($1)", &[&[id, ceo].as_slice()]).await.unwrap().get("marker");
    globals.set("id", id).unwrap();
    let corp: i64 = r.db.query_one("INSERT INTO game.corporations(name,ticker,ceo_id,creator_id,station_id,created) VALUES ('headless-org-' || nextval('game.corporations_corporation_id_seq')::text,'HO' || currval('game.corporations_corporation_id_seq')::text,$1,$1,$2,1) RETURNING corporation_id", &[&ceo, &station]).await.unwrap().get("corporation_id");
    let alliance: i64 = r.db.query_one("INSERT INTO game.alliances(name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('headless-alliance-' || nextval('game.alliances_alliance_id_seq')::text,'HA' || currval('game.alliances_alliance_id_seq')::text,$1,$1,$2,1) RETURNING alliance_id", &[&corp, &ceo]).await.unwrap().get("alliance_id");
    r.db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=$3 WHERE character_id=$1", &[&ceo, &corp, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    let task_r = r.clone();
    let result = tokio::spawn(async move {
        let r = task_r;
        let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
            assert!(lua.load("return pilots.online(id)").eval_async::<bool>().await?);
            let old = r.world.online_session(id).unwrap();
            assert_ne!(old.corporation_id, Some(corp));
            assert_eq!((old.alliance_id, old.corporation_role), (None, 0));
            // Only corporation differs: fresh Identity already sees the new corp,
            // while both the published and durable alliance/role remain unchanged.
            r.db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=0 WHERE character_id=$1", &[&id, &corp]).await?;
            lua.load("pilots.undock(id)").exec_async().await?;
            let corp_only = r.world.online_session(id).unwrap();
            assert_eq!((corp_only.corporation_id, corp_only.alliance_id, corp_only.corporation_role), (Some(corp), None, 0));
            assert_eq!((corp_only.station_id, corp_only.system_id), (None, Some(system)));
            let first_ship = corp_only.ship_id.unwrap();
            let first_ball = r.world.ball(system, first_ship).await.unwrap();
            assert_eq!((first_ball.corp_id, first_ball.alliance_id), (corp, -1));
            assert_eq!(r.pilots.state(id).lock().await.as_ref().unwrap().session, corp_only);
            place_stationary_at_station(&r, id, first_ship, system, station).await?;
            lua.load("pilots.dock(id,station)").exec_async().await?;
            assert_eq!(r.world.online_session(id).unwrap().station_id, Some(station));
            r.db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&corp, &alliance]).await?;
            // Arrange a stale published session; undock must consume all fields returned
            // by the real durable ship transition rather than reject after commit.
            r.db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=$3 WHERE character_id=$1", &[&id, &corp, &infinity_core::consts::ROLE_DIRECTOR]).await?;
            lua.load("pilots.undock(id)").exec_async().await?;
            let undocked = r.world.online_session(id).unwrap();
            assert_eq!((undocked.corporation_id, undocked.alliance_id, undocked.corporation_role), (Some(corp), Some(alliance), infinity_core::consts::ROLE_DIRECTOR));
            assert_eq!((undocked.station_id, undocked.system_id), (None, Some(system)));
            let ship = undocked.ship_id.unwrap();
            let ball = r.world.ball(system, ship).await.unwrap();
            assert_eq!((ball.corp_id, ball.alliance_id), (corp, alliance));
            assert_eq!(r.pilots.state(id).lock().await.as_ref().unwrap().session, undocked);
            // A real CEO RPC removes the corporation from its alliance while the
            // headless pilot is in space, producing the canonical outbox notice.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let _peer = tokio::net::TcpStream::connect(listener.local_addr()?).await?;
            let (stream, address) = listener.accept().await?;
            let cfg = std::sync::Arc::new(crate::config::Config::default());
            let login = crate::handshake::Login { user_id: r.pilots.account, user_name: "test".into(), role: 0, client_id: ceo, session_id: ceo, language_id: "EN".into() };
            let mut admin = crate::client::Client::new(crate::conn::Conn::new(stream, address), cfg, r.db.clone(), r.sde.clone(), r.pilots.hub.clone(), r.world.clone(), login);
            admin.session.apply(vec![("charid", marshal::Value::Int(ceo)), ("corpid", marshal::Value::Int(corp)), ("allianceid", marshal::Value::Int(alliance))]);
            let args = [marshal::Value::Int(corp)];
            let kwargs = marshal::Value::None;
            let binding = marshal::Value::tuple([marshal::Value::Int(alliance), marshal::Value::Bool(true)]);
            let call = services::Call { service: "allianceRegistry", method: "DeleteMember", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
            services::dispatch(&mut admin, &call).await.map_err(|error| anyhow::anyhow!("{error:?}"))?;
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let session = r.world.online_session(id);
                    let ball = r.world.ball(system, ship).await;
                    if session.is_some_and(|session| session.alliance_id.is_none())
                        && ball.is_some_and(|ball| ball.corp_id == corp && ball.alliance_id == -1) { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("alliance notice did not reach headless session and actor");
            assert_eq!(r.pilots.state(id).lock().await.as_ref().unwrap().session.alliance_id, None);
            // Queue a real pending kick while in space; completed dock executes the
            // canonical membership transition and dispatches its real __session outbox.
            r.db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&id]).await?;
            let now = crate::client::filetime_now();
            let admission: Admission = r.db.transaction(
                |tx, _: &mut Outbox| {
                    let world = r.world.clone();
                    Box::pin(async move {
                        if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp], &[ceo, id]).await.map_err(services::db_err)? {
                            return Err(CallError::Internal(anyhow::anyhow!("fixture corporation association changed")));
                        }
                        services::corps::exit::request(tx, &world, id, ceo, corp, now).await
                    })
                },
                |value, _| async move { Ok(value) },
            ).await.map_err(|error: CallError| anyhow::anyhow!("{error:?}"))?;
            assert_eq!(admission, Admission::AwaitSafePresence);
            place_stationary_at_station(&r, id, ship, system, station).await?;
            lua.load("pilots.dock(id,station)").exec_async().await?;
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if r.world.online_session(id).is_some_and(|session| session.corporation_id != Some(corp) && session.station_id == Some(station)) { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("membership session was not delivered");
            let current = r.world.online_session(id).unwrap();
            let actual = crate::dao::corporations::registry::membership(&r.db, id).await?.corporation_id;
            assert_eq!((current.corporation_id, current.alliance_id, current.corporation_role), (Some(actual), None, 0));
            assert_eq!(current.system_id, Some(system));
            assert_eq!(r.pilots.state(id).lock().await.as_ref().unwrap().session, current);
            let old_sender = r.pilots.state(id).lock().await.as_ref().unwrap().notify.clone();
            let before = crate::db::outbox::tests::events(&r.db, "CharacterLoggedOff", id).await.len();
            lua.load("pilots.offline(id)").exec_async().await?;
            let logoffs = crate::db::outbox::tests::events(&r.db, "CharacterLoggedOff", id).await;
            assert_eq!(logoffs.len(), before + 1);
            assert!(matches!(logoffs.last(), Some(crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id }) if (*user_id, *char_id) == (r.pilots.account, id)));
            lua.load("pilots.online(id)").exec_async().await?;
            tokio::time::timeout(Duration::from_secs(10), old_sender.closed()).await.expect("old drain remains alive");
            assert!(old_sender.send(("__session".into(), vec![marshal::Value::tuple([marshal::Value::str("corpid"), marshal::Value::Int(corp)])])).is_err());
            assert_eq!(r.world.online_session(id).unwrap().corporation_id, Some(actual));
            Ok(())
        }.await;
        result.unwrap();
    }).await;
    let _ = lifecycle::offline(r.clone(), id).await;
    let _ = lifecycle::offline(r.clone(), ceo).await;
    r.db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{id}")]).await.unwrap();
    r.db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1", &[&corp]).await.unwrap();
    r.db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    r.db.execute("DELETE FROM game.corp_role_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
    r.db.execute("DELETE FROM game.employment_history WHERE char_id=ANY($1)", &[&[id, ceo].as_slice()]).await.unwrap();
    r.db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&[id, ceo].as_slice(), &marker]).await.unwrap();
    r.db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=ANY($1)", &[&[id, ceo].as_slice()]).await.unwrap();
    r.db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
    r.db.execute("DELETE FROM game.alliance_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
    r.db.execute("DELETE FROM game.alliance_applications WHERE corporation_id=$1 OR alliance_id=$2", &[&corp, &alliance]).await.unwrap();
    r.db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    r.db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    r.db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=ANY($1))", &[&[id, ceo].as_slice()]).await.unwrap();
    r.db.execute("DELETE FROM game.progress_events WHERE subject_character_id=ANY($1)", &[&[id, ceo].as_slice()]).await.unwrap();
    fixture::cleanup(&r.db, id).await;
    fixture::cleanup(&r.db, ceo).await;
    fields.restore(&r).await;
    result.unwrap();
}
