//! Shared test configuration initialization and actor-backed space snapshots.

use std::{future::Future, sync::Once};

/// Poll an async test body while converting a panic into a result so owned
/// database fixtures can be cleaned before the panic is resumed.
pub(crate) async fn catch_scenario(future: impl Future<Output = ()>) -> std::thread::Result<()> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(std::task::Poll::Ready(())) => std::task::Poll::Ready(Ok(())),
        Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
        Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await
}

mod lua_events;

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
        crate::config::init_game(config).expect("initializing test game configuration");
    });
    crate::config::game()
}
