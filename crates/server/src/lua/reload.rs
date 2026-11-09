//! Atomic generation replacement. The old VM remains reachable by every call
//! that obtained its registry before the swap; failed builds leave it untouched.

use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use crate::config::lua::LuaConfig;
use super::{host::Resources, registry::Registry, vm};

pub struct Host {
    pub config: LuaConfig,
    pub resources: Resources,
    current: watch::Sender<Arc<Registry>>,
    reload: tokio::sync::Mutex<()>,
    publication: Mutex<()>,
}

impl Host {
    pub async fn new(config: LuaConfig, resources: Resources) -> anyhow::Result<Self> {
        let registry = vm::build(&config, &resources)?;
        persist(&resources, &registry).await?;
        let (current, _) = watch::channel(Arc::new(registry));
        Ok(Self { config, resources, current, reload: tokio::sync::Mutex::new(()), publication: Mutex::new(()) })
    }

    pub fn snapshot(&self) -> Arc<Registry> {
        self.current.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Registry>> {
        self.current.subscribe()
    }

    pub(crate) fn admit(&self, policy: &super::content::PinnedPolicy) -> Result<super::content::OwnedAdmission, super::content::AdmissionError> {
        let _gate = self.publication.lock().map_err(|e| super::content::AdmissionError::Infrastructure(crate::services::db_err(anyhow::anyhow!("{e}"))))?;
        policy.admit(&self.snapshot())
    }

    pub(super) async fn retained(&self, namespace: &str, version: &str) -> Result<super::content::PinnedPolicy, crate::services::CallError> {
        let row = crate::dao::lua_content::load(&self.resources.db, namespace, version).await.map_err(crate::services::db_err)?
            .ok_or_else(|| crate::services::db_err(anyhow::anyhow!("retained authored policy not found")))?;
        super::content::PinnedPolicy::reopen(self.snapshot(), row, &self.config, &self.resources.sde)
    }

    pub async fn reload(&self) -> anyhow::Result<()> {
        let _guard = self.reload.lock().await;
        let candidate = vm::build(&self.config, &self.resources)?;
        super::content::check_compatible(&self.snapshot().content, &candidate.content)?;
        persist(&self.resources, &candidate).await?;
        let _gate = self.publication.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        self.current.send_replace(Arc::new(candidate));
        Ok(())
    }
}

async fn persist(resources: &Resources, registry: &Registry) -> anyhow::Result<()> {
    let mut rows: Vec<_> = registry.content.packages.values().map(|package| package.row()).collect();
    if rows.is_empty() { return Ok(()); }
    rows.sort_by(|a, b| (&a.namespace, &a.version).cmp(&(&b.namespace, &b.version)));
    let published_at = crate::client::filetime_now();
    resources.db.transaction(|tx, _: &mut ()| {
        let rows = rows.clone();
        Box::pin(async move {
            for row in &rows {
                if !crate::dao::lua_content::store(tx, row).await? {
                    return Err(crate::db::Error::Progress(anyhow::anyhow!("authored policy version already bound to different bytes")));
                }
                crate::dao::lua_content::activations::append_if_changed(tx, row, published_at).await?;
            }
            Ok::<_, crate::db::Error>(())
        })
    }, |(), ()| async { Ok(()) }).await?;
    Ok(())
}


#[cfg(test)]
mod tests;
