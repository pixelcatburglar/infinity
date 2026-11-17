//! The single in-memory catalogue of every published Lua content package and
//! its activation history, loaded once at startup in both enabled and disabled
//! modes. Disabled mode uses it for exact retained-identity validation and
//! recovery, while VM pins and script execution remain unavailable.
//! Operations only clone shared entries; the only runtime writer is the
//! publication step, which installs new packages and appended activations after
//! the publish transaction commits, before the generation swap.
use std::{collections::HashMap, sync::{Arc, Mutex, MutexGuard}};
use anyhow::Context;
use crate::{config::lua::LuaConfig, dao::lua_content::{self, activations::{self, ActivationRow}, PackageRow}, db::Db, sde::Sde, services::{db_err, CallError}};
use super::{content::{self, adaption::Definitions, CanonicalPackage, CanonicalPackages, Package, PinnedPolicy}, registry::Registry, reload::{HostUnavailable, MissingRetainedPackage}};

/// One stored package: the shared row, the VM-free adaptation build and the
/// enabled VM layer's pin, each built at most once.
struct Entry {
    row: Arc<PackageRow>,
    adaption: Option<Arc<Definitions>>,
    policy: Option<PinnedPolicy>,
}

#[derive(Default)]
struct NamespaceEntry {
    /// Retained packages by version; entries are never removed or mutated.
    packages: HashMap<String, Entry>,
    /// Append-only durable activation order, copy-on-write replaced on publish.
    history: Arc<Vec<ActivationRow>>,
}

pub(crate) struct Catalogue {
    namespaces: Mutex<HashMap<String, NamespaceEntry>>,
}

fn missing(namespace: &str, version: &str) -> CallError {
    CallError::Internal(MissingRetainedPackage { namespace: namespace.to_owned(), version: version.to_owned() }.into())
}

impl Catalogue {
    /// Reads both content tables exactly once. Every stored package is kept,
    /// including versions no current script registers; an invalid package or
    /// activation reference fails startup with the precise identity.
    pub(crate) async fn load(db: &Db, config: &LuaConfig, sde: &Sde) -> anyhow::Result<Arc<Self>> {
        let packages = lua_content::load_all(db).await?;
        let activations = activations::load_all(db).await?;
        let mut namespaces: HashMap<String, NamespaceEntry> = HashMap::new();
        for row in packages {
            let identity = format!("{}/{}", row.namespace, row.version);
            let adaption = if row.namespace == "ship_adaption" {
                Some(Arc::new(content::decode_retained_adaption(&row, config, sde)
                    .with_context(|| format!("retained adaptation package {identity}"))?))
            } else {
                content::verify_row(&row, config.data_file_limit_bytes)
                    .with_context(|| format!("retained package {identity}"))?;
                None
            };
            namespaces.entry(row.namespace.clone()).or_default()
                .packages.insert(row.version.clone(), Entry { row: Arc::new(row), adaption, policy: None });
        }
        let mut groups: HashMap<String, Vec<ActivationRow>> = HashMap::new();
        for row in activations {
            groups.entry(row.namespace.clone()).or_default().push(row);
        }
        for (namespace, mut rows) in groups {
            rows.sort_by_key(|row| row.ordinal);
            let entry = namespaces.entry(namespace.to_owned()).or_default();
            let mut previous = 0;
            for row in rows {
                let package = entry.packages.get(&row.version)
                    .with_context(|| format!("retained activation {} references unknown package {}/{}", row.ordinal, namespace, row.version))?;
                anyhow::ensure!(row.ordinal > previous && row.published_at > 0 && row.fingerprint == package.row.fingerprint,
                    "invalid retained activation {} for {}/{}", row.ordinal, namespace, row.version);
                previous = row.ordinal;
                Arc::make_mut(&mut entry.history).push(row);
            }
        }
        Ok(Arc::new(Self { namespaces: Mutex::new(namespaces) }))
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, NamespaceEntry>> {
        crate::poison::lock_or_exit(self.namespaces.lock())
    }
    /// Check one retained package identity without cloning or decoding it.
    pub(super) fn validate_identity(&self, namespace: &str, version: &str) -> Result<(), CallError> {
        let namespaces = self.lock();
        namespaces.get(namespace).and_then(|group| group.packages.get(version))
            .map(|_| ())
            .ok_or_else(|| missing(namespace, version))
    }


    /// The VM-free adaptation definitions built at load; zero SQL, zero decode.
    pub(crate) fn adaption(&self, namespace: &str, version: &str) -> Result<Arc<Definitions>, CallError> {
        let namespaces = self.lock();
        let entry = namespaces.get(namespace).and_then(|group| group.packages.get(version))
            .ok_or_else(|| missing(namespace, version))?;
        entry.adaption.clone()
            .ok_or_else(|| db_err(anyhow::anyhow!("retained package {namespace}/{version} is not adaptation content")))
    }

    /// The VM-layer pin of a retained package; unavailable until the enabled
    /// host completes the pin layer, and never a database fallback.
    pub(super) fn resolve(&self, namespace: &str, version: &str) -> Result<PinnedPolicy, CallError> {
        let namespaces = self.lock();
        let entry = namespaces.get(namespace).and_then(|group| group.packages.get(version))
            .ok_or_else(|| missing(namespace, version))?;
        entry.policy.clone().ok_or_else(|| CallError::Internal(HostUnavailable.into()))
    }

    /// Immutable activation history snapshot for one namespace; empty for a
    /// namespace that never published.
    pub(super) fn activations(&self, namespace: &str) -> Result<Arc<Vec<ActivationRow>>, CallError> {
        Ok(self.lock().get(namespace).map(|entry| entry.history.clone()).unwrap_or_default())
    }

    /// Head ordinal of one namespace history; the namespace version used for
    /// publication-stability checks.
    pub(super) fn head(&self, namespace: &str) -> Result<Option<i64>, CallError> {
        Ok(self.lock().get(namespace).and_then(|entry| entry.history.last().map(|row| row.ordinal)))
    }

    /// Snapshot of every stored package identity for a generation build:
    /// the shared row, the decoded adaptation definitions and the pinned
    /// built package keyed by (namespace, version). Capturing a flat map
    /// keeps no catalogue back-reference.
    pub(super) fn canonical(&self) -> anyhow::Result<Arc<CanonicalPackages>> {
        Ok(Arc::new(crate::poison::lock_or_exit(self.namespaces.lock()).iter()
            .flat_map(|(namespace, group)| group.packages.iter()
                .map(|(version, entry)| ((namespace.clone(), version.clone()), CanonicalPackage {
                    row: entry.row.clone(),
                    package: entry.policy.as_ref().map(|policy| policy.package().clone()),
                    adaption: entry.adaption.clone(),
                })))
            .collect()))
    }

    /// Entries still missing the VM-layer pin; the enabled host builds them
    /// once during startup.
    pub(super) fn unpinned(&self) -> anyhow::Result<Vec<(String, String, Arc<PackageRow>, Option<Arc<Definitions>>)>> {
        Ok(crate::poison::lock_or_exit(self.namespaces.lock()).iter()
            .flat_map(|(namespace, group)| group.packages.iter()
                .filter(|(_, entry)| entry.policy.is_none())
                .map(|(version, entry)| (namespace.clone(), version.clone(), entry.row.clone(), entry.adaption.clone())))
            .collect())
    }

    /// Installs committed publication work in one short critical section:
    /// appended activation rows, the new generation's current packages and the
    /// prepared retained pins, then rebinds every retained pin to the new
    /// registry so no superseded claims VM stays referenced. Runs only after
    /// the publish transaction commits and before the generation swap.
    pub(super) fn install(&self, registry: &Arc<Registry>, current: Vec<Arc<Package>>, appended: Vec<ActivationRow>, prepared: Vec<PinnedPolicy>) -> anyhow::Result<()> {
        let mut namespaces = crate::poison::lock_or_exit(self.namespaces.lock());
        for row in appended {
            let entry = namespaces.entry(row.namespace.clone()).or_default();
            anyhow::ensure!(entry.history.last().is_none_or(|last| row.ordinal > last.ordinal),
                "retained activation ordinal out of order for {}", row.namespace);
            Arc::make_mut(&mut entry.history).push(row);
        }
        for package in current {
            namespaces.entry(package.namespace().to_owned()).or_default()
                .packages.insert(package.version().to_owned(), Entry {
                    row: package.row.clone(),
                    adaption: package.adaption().cloned(),
                    policy: Some(PinnedPolicy::pin_retained(registry.clone(), package)),
                });
        }
        for policy in prepared {
            let entry = namespaces.get_mut(policy.namespace())
                .and_then(|group| group.packages.get_mut(policy.version()))
                .ok_or_else(|| anyhow::anyhow!("retained authored policy {}/{} not found", policy.namespace(), policy.version()))?;
            entry.policy = Some(policy);
        }
        for namespace in namespaces.values_mut() {
            for entry in namespace.packages.values_mut() {
                if let Some(policy) = &entry.policy {
                    if !policy.pinned_on(registry) {
                        entry.policy = Some(PinnedPolicy::pin_retained(registry.clone(), policy.package().clone()));
                    }
                }
            }
        }
        Ok(())
    }
}
