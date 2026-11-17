//! Atomic generation replacement. The old VM remains reachable by every call
//! that obtained its registry before the swap; failed builds leave it untouched.
//! The shared startup catalogue holds every published package and activation;
//! a real development-index activation change notifies the indices scheduler
//! only after the new generation is published.

use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use crate::{config::lua::LuaConfig, dao::lua_content::activations::ActivationRow};
use super::{catalogue::Catalogue, host::Resources, registry::Registry, vm};

/// A referenced retained package is absent from `game.lua_content_versions`.
/// Corrupt hashes, schema or compiler identities are distinct errors, never this.
#[derive(Debug)]
pub(crate) struct MissingRetainedPackage {
    pub namespace: String,
    pub version: String,
}
impl std::fmt::Display for MissingRetainedPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retained authored policy {}/{} not found", self.namespace, self.version)
    }
}
impl std::error::Error for MissingRetainedPackage {}

/// No usable Lua host exists to reopen retained packages.
#[derive(Debug)]
pub(crate) struct HostUnavailable;
impl std::fmt::Display for HostUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Lua host is unavailable") }
}
impl std::error::Error for HostUnavailable {}

pub struct Host {
    pub config: LuaConfig,
    pub resources: Resources,
    catalogue: Arc<Catalogue>,
    current: watch::Sender<Arc<Registry>>,
    reload: Arc<tokio::sync::Mutex<()>>,
    publication: Mutex<()>,
}

impl Host {
    /// Builds the current generation against the shared startup catalogue and
    /// prepares the retained pin layer for every stored package before this
    /// host becomes usable. Current packages are pinned from the generation
    /// build itself and never recompiled; a failed historical build aborts
    /// before the publish transaction, never after it.
    pub async fn new(config: LuaConfig, resources: Resources, catalogue: Arc<Catalogue>) -> anyhow::Result<Self> {
        let registry = Arc::new(vm::build(&config, &resources, &catalogue)?);
        let mut prepared = Vec::new();
        for (namespace, version, row, adaption) in catalogue.unpinned()? {
            if registry.content.packages.get(&namespace).is_some_and(|package| package.version() == version && package.row == row) {
                continue;
            }
            prepared.push(super::content::PinnedPolicy::reopen(registry.clone(), row, adaption, &config, &resources.sde).map_err(|error| anyhow::anyhow!("{error:?}"))?);
        }
        let appended = publish(&resources, &registry, crate::client::filetime_now()).await?;
        catalogue.install(&registry, registry.content.packages.values().cloned().collect(), appended, prepared)?;
        let (current, _) = watch::channel(registry);
        Ok(Self { config, resources, catalogue, current, reload: Arc::new(tokio::sync::Mutex::new(())), publication: Mutex::new(()) })
    }

    pub fn snapshot(&self) -> Arc<Registry> {
        self.current.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Registry>> {
        self.current.subscribe()
    }

    pub(crate) fn catalogue(&self) -> &Arc<Catalogue> {
        &self.catalogue
    }

    /// Retained-policy handle for prepared consumers and this host's own
    /// publication notification: the same catalogue plus the reload guard.
    pub(crate) fn policies(&self) -> super::RetainedPolicies {
        super::RetainedPolicies { catalogue: self.catalogue.clone(), reload: self.reload.clone() }
    }

    pub(crate) fn admit(&self, policy: &super::content::PinnedPolicy) -> Result<super::content::OwnedAdmission, super::content::AdmissionError> {
        let _gate = crate::poison::lock_or_exit(self.publication.lock());
        policy.admit(&self.snapshot())
    }

    pub async fn reload(&self) -> anyhow::Result<()> {
        self.publish_at(crate::client::filetime_now()).await
    }

    /// The one publication path: build the candidate, run the publish
    /// transaction, install the committed packages and activations into the
    /// shared catalogue, then swap the generation. A failure before COMMIT
    /// leaves the old generation and catalogue untouched.
    async fn publish_at(&self, published_at: i64) -> anyhow::Result<()> {
        let _guard = self.reload.lock().await;
        let candidate = Arc::new(vm::build(&self.config, &self.resources, &self.catalogue)?);
        super::content::check_compatible(&self.snapshot().content, &candidate.content)?;
        let appended = publish(&self.resources, &candidate, published_at).await?;
        let mut changed: Vec<String> = appended.iter().map(|row| row.namespace.clone()).collect();
        changed.sort();
        changed.dedup();
        {
            let _gate = crate::poison::lock_or_exit(self.publication.lock());
            self.catalogue.install(&candidate, candidate.content.packages.values().cloned().collect(), appended, Vec::new())?;
            self.current.send_replace(candidate);
        }
        if changed.iter().any(|namespace| namespace == super::content::indices::NAMESPACE) {
            // The activation is already committed; a failed notification never
            // means the reload kept the old generation.
            if let Err(error) = crate::services::sovereignty::indices::jobs::policy_published(&self.resources.db, &self.resources.world, &self.policies()).await {
                tracing::warn!("development-index policy publication notification failed: {error:#?}");
            }
        }
        Ok(())
    }
}

/// Persists the generation's packages inside one publication transaction and
/// returns exactly the activation rows that were appended. The catalogue is
/// updated by the caller only after COMMIT.
async fn publish(resources: &Resources, registry: &Registry, published_at: i64) -> anyhow::Result<Vec<ActivationRow>> {
    let mut rows: Vec<_> = registry.content.packages.values().map(|package| package.row.as_ref().clone()).collect();
    if rows.is_empty() { return Ok(Vec::new()); }
    rows.sort_by(|a: &crate::dao::lua_content::PackageRow, b: &crate::dao::lua_content::PackageRow| (&a.namespace, &a.version).cmp(&(&b.namespace, &b.version)));
    resources.db.transaction(|tx, _: &mut ()| {
        let rows = rows.clone();
        Box::pin(async move {
            crate::dao::lua_content::activations::publish(tx, &rows, published_at).await
        })
    }, |appended: Vec<ActivationRow>, ()| async move { Ok(appended) }).await
    .map_err(Into::into)
}


#[cfg(all(test, any(feature = "tests-lua", feature = "tests-content", feature = "tests-gateway")))]
pub(crate) mod tests;
