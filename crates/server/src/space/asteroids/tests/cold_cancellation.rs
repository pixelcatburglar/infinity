//! Cold system cancellation at a PostgreSQL pre-commit barrier.
//! This exercises the server's asteroid persistence and actor projection, not a client protocol.
use super::*;
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;
use tracing::field::{Field as TraceField, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::{layer::Context, prelude::*, Layer};

// Test failure bound for each database barrier, not a synchronization delay.
const BARRIER_TIMEOUT: Duration = Duration::from_secs(30);

struct NoticeLayer {
    token: String,
    tx: tokio::sync::mpsc::UnboundedSender<()>,
}

struct NoticeVisitor<'a> { token: &'a str, matched: bool, target: bool }
impl Visit for NoticeVisitor<'_> {
    fn record_str(&mut self, field: &TraceField, value: &str) {
        match field.name() {
            "message" => self.matched = value.contains(&format!("NOTICE: {}", self.token)),
            "log.target" => self.target |= value == "tokio_postgres::connection",
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &TraceField, value: &dyn std::fmt::Debug) {
        let value = format!("{value:?}");
        self.record_str(field, value.trim_matches('"'));
    }
}
impl<S: Subscriber> Layer<S> for NoticeLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut visitor = NoticeVisitor {
            token: &self.token,
            matched: false,
            target: event.metadata().target() == "tokio_postgres::connection",
        };
        event.record(&mut visitor);
        if visitor.matched && visitor.target { let _ = self.tx.send(()); }
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn cancelled_cold_world_caller_cannot_publish_before_retry_commits() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..cfg.clone()
    }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let mut selected = None;
    for row in sde.map.systems() {
        let system = row.solar_system_id;
        let Ok(inputs) = cold_inputs(&sde, system) else { continue };
        if !inputs.iter().any(|(_, _, input)| !input.rocks.is_empty()) { continue; }
        let clean: bool = db.query_one("SELECT NOT EXISTS (SELECT 1 FROM game.asteroids WHERE system_id=$1) AND NOT EXISTS (SELECT 1 FROM game.asteroid_fields WHERE system_id=$1) AND NOT EXISTS (SELECT 1 FROM game.sov_systems WHERE system_id=$1) AND NOT EXISTS (SELECT 1 FROM game.items WHERE location_id=$1 AND x IS NOT NULL) AS clean", &[&system]).await.unwrap().get("clean");
        if !clean { continue; }
        let mut unused = true;
        for (field, _, _) in &inputs {
            if dao::field(&db, *field).await.unwrap().is_some() { unused = false; break; }
        }
        if unused {
            selected = Some((system, inputs.iter().map(|(field, _, _)| *field).collect::<Vec<_>>()));
            break;
        }
    }
    let (system, fields) = selected.expect("dedicated SDE database needs a clean generating system");
    let key: i64 = db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let name = format!("asteroid_cold_barrier_{key}");
    let token = format!("asteroid_cold_notice_{key}");
    let work_name = name.clone();
    let result = tokio::task::spawn_blocking(move || {
        let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        let subscriber = tracing_subscriber::registry().with(NoticeLayer { token: token.clone(), tx });
        let _guard = subscriber.set_default();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            tokio::time::timeout(Duration::from_secs(120), async move {
                let work_db = Db::connect(&crate::config::deployment::DatabaseConfig {
                    schema_on_start: false, ..cfg.clone()
                }).await.unwrap();
                let (control, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
                tokio::spawn(async move { connection.await.unwrap(); });
                control.query_one("SELECT pg_advisory_lock($1)", &[&key]).await.unwrap();
                let field = fields[0];
                control.batch_execute(&format!("CREATE FUNCTION game.{work_name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.system_id = {system} AND NEW.field_id = {field} THEN RAISE NOTICE '{token}'; PERFORM pg_advisory_xact_lock({key}); END IF; RETURN NEW; END $$; CREATE TRIGGER {work_name} BEFORE INSERT ON game.asteroid_fields FOR EACH ROW EXECUTE FUNCTION game.{work_name}()")).await.unwrap();
                let world = World::new(work_db.clone(), sde.clone(), 1);
                let first_world = world.clone();
                let first = tokio::spawn(async move { first_world.system(system).await });
                tokio::time::timeout(BARRIER_TIMEOUT, notices.recv()).await
                    .expect("first pre-commit notice timed out")
                    .expect("first cold transaction must reach the pre-commit trigger");
                assert!(!first.is_finished());
                assert!(world.running(system).is_none());
                assert!(world.asteroid_actor(system).is_none());
                assert_eq!(world.with(system, |_| ()).await, None);
                assert!(dao::load(&work_db, system).await.unwrap().is_empty());
                assert!(dao::fields(&work_db, system, &[]).await.unwrap().is_empty());
                let second = world.system(system);
                tokio::pin!(second);
                assert!(poll_fn(|cx| Poll::Ready(second.as_mut().poll(cx).is_pending())).await,
                    "second caller must wait for the first caller's market guard");
                first.abort();
                assert!(matches!(first.await, Err(error) if error.is_cancelled()));
                control.query_one("SELECT pg_advisory_unlock($1)", &[&key]).await.unwrap();
                // The exclusive lock waits for the canceled transaction's queued rollback,
                // including completion of its advisory-blocked marker insertion.
                tokio::time::timeout(BARRIER_TIMEOUT, control.batch_execute(
                    "BEGIN; LOCK TABLE game.asteroid_fields IN ACCESS EXCLUSIVE MODE; ROLLBACK;"
                )).await.expect("rollback table-lock fence timed out").unwrap();
                assert!(world.running(system).is_none());
                assert!(world.asteroid_actor(system).is_none());
                assert_eq!(world.with(system, |_| ()).await, None);
                assert!(dao::load(&work_db, system).await.unwrap().is_empty());
                assert!(dao::fields(&work_db, system, &[]).await.unwrap().is_empty());
                control.query_one("SELECT pg_advisory_lock($1)", &[&key]).await.unwrap();
                tokio::time::timeout(BARRIER_TIMEOUT, async {
                    tokio::select! {
                        biased;
                        result = &mut second => panic!("second caller completed before its pre-commit barrier: {result:?}"),
                        notice = notices.recv() => assert!(notice.is_some(), "second cold transaction must reach the trigger"),
                    }
                }).await.expect("second pre-commit notice timed out");
                assert!(world.running(system).is_none());
                assert!(dao::load(&work_db, system).await.unwrap().is_empty());
                control.query_one("SELECT pg_advisory_unlock($1)", &[&key]).await.unwrap();
                second.await.unwrap();
                let projection = world.with(system, |s| {
                    let mut rocks = s.rocks.iter().map(|(&id, rock)| {
                        let ball = s.ball(id).expect("every rock has a visible ball");
                        assert_eq!(s.rock_type(id), Some(rock.type_id));
                        (id, rock.field, rock.type_id, rock.quantity, ball.radius, ball.pos)
                    }).collect::<Vec<_>>();
                    rocks.sort_by_key(|rock| rock.0);
                    rocks
                }).await.unwrap();
                assert!(!projection.is_empty());
                world.system(system).await.unwrap();
                let recreated = World::new(work_db.clone(), sde, 1);
                recreated.system(system).await.unwrap();
                let reloaded = recreated.with(system, |s| {
                    let mut ids = s.rocks.keys().copied().collect::<Vec<_>>();
                    ids.sort_unstable();
                    ids
                }).await.unwrap();
                let rows = dao::load(&work_db, system).await.unwrap();
                let markers = dao::fields(&work_db, system, &[]).await.unwrap();
                assert_eq!(projection.len(), rows.len());
                assert_eq!(reloaded, projection.iter().map(|r| r.0).collect::<Vec<_>>());
                for (projected, row) in projection.iter().zip(&rows) {
                    assert_eq!(*projected, (row.id, row.field, row.type_id, row.quantity,
                        row.radius.unwrap_or(STORED_RADIUS_FALLBACK), [row.x, row.y, row.z]));
                }
                let mut marker_fields = markers.iter().map(|m| {
                    assert_eq!(m.system, system);
                    assert_eq!(m.depleted_at, None);
                    m.field
                }).collect::<Vec<_>>();
                marker_fields.sort_unstable();
                let mut expected_fields = fields;
                expected_fields.sort_unstable();
                assert_eq!(marker_fields, expected_fields);
            }).await.expect("cold cancellation scenario timed out");
        });
        drop(runtime);
    }).await;
    // Runtime teardown drops all actors and connections before independent cleanup.
    db.execute(&format!("DROP TRIGGER IF EXISTS {name} ON game.asteroid_fields"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION IF EXISTS game.{name}()"), &[]).await.unwrap();
    db.execute("DELETE FROM game.asteroids WHERE system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1", &[&system]).await.unwrap();
    result.unwrap();
}
