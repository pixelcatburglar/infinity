//! Atomic generation replacement. The old VM remains reachable by every call
//! that obtained its registry before the swap; failed builds leave it untouched.

use std::sync::Arc;
use tokio::sync::watch;
use crate::config::lua::LuaConfig;
use super::{host::Resources, registry::Registry, vm};

pub struct Host {
    pub config: LuaConfig,
    pub resources: Resources,
    current: watch::Sender<Arc<Registry>>,
    reload: tokio::sync::Mutex<()>,
}

impl Host {
    pub async fn new(config: LuaConfig, resources: Resources) -> anyhow::Result<Self> {
        let registry = vm::build(&config, &resources)?;
        let (current, _) = watch::channel(Arc::new(registry));
        Ok(Self { config, resources, current, reload: tokio::sync::Mutex::new(()) })
    }

    pub fn snapshot(&self) -> Arc<Registry> {
        self.current.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Registry>> {
        self.current.subscribe()
    }

    pub async fn reload(&self) -> anyhow::Result<()> {
        let _guard = self.reload.lock().await;
        self.current.send_replace(Arc::new(vm::build(&self.config, &self.resources)?));
        Ok(())
    }
}


#[cfg(test)]
mod tests;
