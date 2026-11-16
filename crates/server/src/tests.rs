//! Shared test configuration initialization and actor-backed space snapshots.

use std::sync::Once;
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-inventory", feature = "tests-skills", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-slash", feature = "tests-services", feature = "tests-space", feature = "tests-lua", feature = "tests-storage", feature = "tests-gateway"))]
use std::future::Future;

#[cfg(not(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-inventory", feature = "tests-skills", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-slash", feature = "tests-services", feature = "tests-space", feature = "tests-lua", feature = "tests-storage", feature = "tests-gateway", feature = "tests-sde", feature = "tests-runtime")))]
compile_error!("No server test domain selected; enable default features, tests-all, or at least one tests-<domain> feature listed in docs/development.md.");

/// Poll an async test body while converting a panic into a result so owned
/// database fixtures can be cleaned before the panic is resumed.
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-inventory", feature = "tests-skills", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-slash", feature = "tests-services", feature = "tests-space", feature = "tests-lua", feature = "tests-storage", feature = "tests-gateway"))]
pub(crate) async fn catch_scenario(future: impl Future<Output = ()>) -> std::thread::Result<()> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(std::task::Poll::Ready(())) => std::task::Poll::Ready(Ok(())),
        Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
        Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await
}

#[cfg(feature = "tests-lua")]
mod lua_events;

#[cfg(feature = "tests-runtime")]
mod startup;

#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-inventory", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-slash", feature = "tests-services", feature = "tests-space", feature = "tests-lua", feature = "tests-runtime"))]
impl crate::space::World {
    pub async fn ball(&self, system: i64, id: i64) -> Option<crate::space::Ball> {
        self.with(system, move |s| s.ball(id).cloned()).await.flatten()
    }
}

pub(crate) fn init_config() -> &'static crate::config::game::GameConfig {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mut config = crate::config::game::GameConfig::default();
        config.space.collisions = true;
        // Exact test processes that exercise the production idle-stop control
        // channel opt in here; every other process keeps default behavior.
        if std::env::var_os("INFINITY_TEST_ENABLE_IDLE_STOP").is_some() {
            config.gm.performance_testing = true;
        }
        crate::config::init_game(config).expect("initializing test game configuration");
    });
    crate::config::game()
}

/// Load the static dataset once per test process; every caller receives a
/// clone of the same `Arc`. The cell holds owned data only: no `Db` or runtime
/// is retained, so the shared `Arc` outlives the loader's runtime. A failed or
/// cancelled initializer publishes nothing and a later call retries the load.
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-inventory", feature = "tests-skills", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-slash", feature = "tests-services", feature = "tests-space", feature = "tests-lua", feature = "tests-storage", feature = "tests-gateway", feature = "tests-runtime"))]
pub(crate) async fn shared_sde(db: &crate::db::Db) -> std::sync::Arc<crate::sde::Sde> {
    static SDE: tokio::sync::OnceCell<std::sync::Arc<crate::sde::Sde>> = tokio::sync::OnceCell::const_new();
    SDE.get_or_try_init(|| async { crate::sde::Sde::load(db).await.map(std::sync::Arc::new) }).await.unwrap().clone()
}

#[cfg(feature = "tests-runtime")]
#[test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
fn shared_sde_arc_survives_runtime_exit_across_runtimes() {
    let load = || tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        tokio::time::pause(); let cfg = crate::config::tests::test_database(); tokio::time::resume();
        assert!(!cfg.schema_on_start);
        let db = crate::db::Db::connect(&cfg).await.unwrap();
        db.query_one("SELECT 1", &[]).await.unwrap();
        shared_sde(&db).await
    });
    let other = std::thread::spawn(load);
    let (first, second) = (load(), other.join().unwrap());
    let third = load();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert!(std::sync::Arc::ptr_eq(&first, &third));
}
