//! One process-owned Lua host and runtime for the Hub integration acceptance.
//! Run this exact ignored test alone: the production Lua host is a OnceLock.
mod fixture;
mod entosis;
mod historical;
mod scenarios;
mod native;

use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
#[ignore = "requires a dedicated initialized INFINITY_TEST_DATABASE_URL and real SDE; run this exact test in its own process"]
fn hub_lifecycle_governance_integration() {
    crate::tests::init_config();
    let state = Arc::new(Mutex::new(fixture::Recorded::default()));
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let shared = state.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(async {
        let mut task = tokio::spawn(async move {
            let fixture = fixture::Fixture::new(shared).await;
            scenarios::run(&fixture).await;
        });
        let result = tokio::time::timeout(Duration::from_secs(240), &mut task).await;
        if result.is_err() { task.abort(); let _ = task.await; }
        let world = state.lock().unwrap().world.clone();
        if let Some(world) = world { crate::space::sovereignty::close(&world); }
        let _fence = crate::services::market::lock().await;
        match result {
            Ok(Ok(())) => {},
            Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Ok(Err(error)) => panic!("integration task cancelled: {error}"),
            Err(_) => panic!("Hub integration scenario timed out"),
        }
    })));
    // Dropping the owned runtime cancels actor and deferred tasks before fixture deletion.
    drop(runtime);
    let ids = state.lock().unwrap().ids;
    let cleanup_runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup_runtime.block_on(fixture::cleanup(ids))));
    if let Err(error) = cleanup {
        eprintln!("Hub integration fixture cleanup failed: {error:?}");
        if let Err(original) = result { std::panic::resume_unwind(original); }
        std::panic::resume_unwind(error);
    }
    if let Err(error) = result { std::panic::resume_unwind(error); }
}
