//! Shared initialization for unit tests using the default game configuration.

use std::sync::Once;

mod lua_events;

pub(crate) fn init_config() -> &'static crate::config::game::GameConfig {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mut config = crate::config::game::GameConfig::default();
        config.space.collisions = true;
        crate::config::init_game(config).expect("initializing test game configuration");
    });
    crate::config::game()
}
