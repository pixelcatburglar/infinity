//! Ordered mixed post-commit dispatch and error propagation.
use std::cell::RefCell;
use crate::{db::outbox::{Notification, Outbox}, services::{CallError, effects::online::{self, OnlineChanged}}};
use marshal::Value;

enum Mixed { Online(OnlineChanged), Other(&'static str) }

fn split(entry: Mixed) -> Result<OnlineChanged, Mixed> {
    match entry { Mixed::Online(change) => Ok(change), other => Err(other) }
}

fn event(notification: Notification) -> String {
    match notification {
        Notification::DirectClient { event, .. } | Notification::Pilot { event, .. } => event,
        _ => panic!("unexpected notification"),
    }
}

async fn world() -> std::sync::Arc<crate::space::World> {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    crate::space::World::new(db, sde, 1)
}

fn entries() -> Outbox<Mixed> {
    let mut out = Outbox::default();
    out.operation(Mixed::Other("first"));
    out.notify_client("before", vec![]);
    out.operation(Mixed::Online(OnlineChanged::StoredFitting { owner: 1, changes: vec![Value::Int(1)] }));
    out.operation(Mixed::Other("second"));
    out.notify_client("after", vec![]);
    out
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn mixed_dispatch_preserves_order_and_stops_on_errors() {
    let world = world().await;
    let calls = RefCell::new(Vec::new());
    let result = online::dispatch_mixed_entries(7, entries(), &world, split,
        |entry| { let Mixed::Other(name) = entry else { unreachable!() }; calls.borrow_mut().push(name.to_string()); Ok(()) },
        |notification| { calls.borrow_mut().push(event(notification)); Ok(()) });
    assert_eq!(result.unwrap(), 7);
    assert_eq!(*calls.borrow(), ["first", "before", "OnModuleAttributeChanges", "second", "after"]);

    calls.borrow_mut().clear();
    let result = online::dispatch_mixed_entries((), entries(), &world, split,
        |entry| { let Mixed::Other(name) = entry else { unreachable!() }; calls.borrow_mut().push(name.to_string());
            if name == "second" { Err(crate::services::notify_error("dispatch failure")) } else { Ok(()) } },
        |notification| { calls.borrow_mut().push(event(notification)); Ok(()) });
    assert!(result.is_err());
    assert_eq!(*calls.borrow(), ["first", "before", "OnModuleAttributeChanges", "second"]);

    calls.borrow_mut().clear();
    let result = online::dispatch_mixed_entries((), entries(), &world, split,
        |entry| { let Mixed::Other(name) = entry else { unreachable!() }; calls.borrow_mut().push(name.to_string()); Ok(()) },
        |notification| { let name = event(notification); calls.borrow_mut().push(name.clone());
            if name == "OnModuleAttributeChanges" { Err(crate::services::notify_error("dispatch failure")) } else { Ok(()) } });
    assert!(result.is_err());
    assert_eq!(*calls.borrow(), ["first", "before", "OnModuleAttributeChanges"]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn ordinary_dispatch_warns_and_continues_after_notify_failure() {
    let world = world().await;
    let mut out: Outbox<OnlineChanged> = Outbox::default();
    out.notify_client("first", vec![]);
    out.operation(OnlineChanged::StoredFitting { owner: 1, changes: vec![Value::Int(1)] });
    out.notify_client("last", vec![]);
    let mut calls = Vec::new();
    let result: Result<_, CallError> = online::dispatch_entries(9, out, &world, |notification| {
        let name = event(notification);
        calls.push(name.clone());
        if name == "first" { Err(crate::services::notify_error("dispatch failure")) } else { Ok(()) }
    });
    assert_eq!(result.unwrap(), 9);
    assert_eq!(calls, ["first", "OnModuleAttributeChanges", "last"]);
}
