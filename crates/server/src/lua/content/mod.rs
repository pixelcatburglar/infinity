//! Immutable sovereignty, development-index, adaptation and daily content captured per Lua generation.
//! Adaptation and index data come from a validated deployment bundle and daily data is authored JSON;
//! retained daily packages preserve references validated at activation across SDE changes.
//! Gateway handlers, progress settlement, node and mission gameplay are not implemented here.

pub(crate) mod adaption;
pub(super) mod bundle;
pub(crate) mod daily_goals;
pub(crate) mod indices;
mod policy;
pub(crate) mod protocol;

use std::{collections::HashMap, path::Path, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};
use mlua::{Lua, Table, Value};
use sha2::{Digest, Sha256};
use crate::{config::lua::LuaConfig, hash::sha256_hex, services::{CallError, db_err}};
use super::registry::Registry;

pub(crate) use policy::{PolicyInput, PolicyPlan, Rule};

/// Server-defined identity of the closed sovereignty policy compiler.
const COMPILER_IDENTITY: &str = "sovereignty-policy-v1";
/// Server-defined identity of the adaptation catalogue compiler.
const ADAPTION_COMPILER_IDENTITY: &str = "ship-adaption-chip-v1";
const DAILY_GOALS_COMPILER_IDENTITY: &str = "daily-goals-policy-v1";
/// Server-defined identity of the closed development-index policy compiler.
const INDICES_COMPILER_IDENTITY: &str = "sovereignty-indices-v1";
/// Server safety ceiling on message keys and the number of catalogue entries.
const MESSAGE_CATALOGUE_LIMIT: usize = 256;
/// JSON integer-array representation overhead for the seven bounded files and metadata.
const RETAINED_SIZE_FACTOR: usize = 40;

enum PackageDomain {
    Sovereignty(policy::Values),
    Adaption(Arc<adaption::Definitions>),
    DailyGoals(Arc<daily_goals::model::Definitions>),
    SovereigntyIndices(Arc<indices::Definitions>),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AdaptionData {
    bundle: bundle::Bundle,
    messages_source: Vec<u8>,
    messages: std::collections::BTreeMap<String, String>,
    catalogue: adaption::Definitions,
}

#[derive(serde::Serialize)]
struct AdaptionDataRef<'a> {
    bundle: &'a bundle::Bundle,
    messages_source: &'a [u8],
    messages: &'a std::collections::BTreeMap<String, String>,
    catalogue: &'a adaption::Definitions,
}

#[derive(Default)]
pub(super) struct ContentDefinitions {
    pub(super) packages: HashMap<String, Arc<Package>>,
}

pub(super) fn check_compatible(previous: &ContentDefinitions, next: &ContentDefinitions) -> anyhow::Result<()> {
    if let Some(old) = previous.packages.get("ship_adaption") {
        let new = next.packages.get("ship_adaption").ok_or_else(|| anyhow::anyhow!("adaptation catalogue was removed"))?;
        let (PackageDomain::Adaption(old), PackageDomain::Adaption(new)) = (&old.domain, &new.domain) else {
            anyhow::bail!("invalid adaptation catalogue domain");
        };
        anyhow::ensure!(new.compatible(old), "adaptation catalogue removed a chip currency");
    }
    Ok(())
}

pub(super) struct Package {
    /// Canonical stored identity and bytes, shared with the startup catalogue.
    pub(super) row: Arc<crate::dao::lua_content::PackageRow>,
    pub(super) config: LuaConfig,
    domain: PackageDomain,
}

impl Package {
    pub(super) fn namespace(&self) -> &str { &self.row.namespace }
    pub(super) fn version(&self) -> &str { &self.row.version }
    pub(super) fn adaption(&self) -> Option<&Arc<adaption::Definitions>> {
        match &self.domain { PackageDomain::Adaption(definitions) => Some(definitions), _ => None }
    }
}

#[derive(Clone)]
pub(crate) struct PinnedPolicy {
    registry: Arc<Registry>,
    package: Arc<Package>,
    retained: bool,
}

impl PinnedPolicy {
    pub(super) fn pin(registry: Arc<Registry>, namespace: &str) -> Result<Self, CallError> {
        let package = registry.content.packages.get(namespace).cloned()
            .ok_or_else(|| db_err(anyhow::anyhow!("unknown authored content namespace {namespace}")))?;
        Ok(Self { registry, package, retained: false })
    }
    pub(crate) fn namespace(&self) -> &str { &self.package.row.namespace }
    pub(crate) fn same_package(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.registry, &other.registry) && Arc::ptr_eq(&self.package, &other.package)
    }
    pub(crate) fn version(&self) -> &str { &self.package.row.version }
    pub(crate) fn fingerprint(&self) -> &[u8; 32] {
        self.package.row.fingerprint.as_slice().try_into().expect("validated package fingerprint")
    }
    pub(crate) fn checkpoint_interval_ms(&self) -> Result<u64, CallError> {
        match &self.package.domain {
            PackageDomain::Sovereignty(values) => Ok(values.checkpoint_ms),
            _ => Err(db_err(anyhow::anyhow!("checkpoint interval requires sovereignty policy"))),
        }
    }
    pub(crate) fn adaption(&self) -> Option<&Arc<adaption::Definitions>> {
        self.package.adaption()
    }
    pub(crate) fn daily_goals(&self) -> Option<&Arc<daily_goals::model::Definitions>> {
        match &self.package.domain { PackageDomain::DailyGoals(definitions) => Some(definitions), _ => None }
    }
    pub(crate) fn indices(&self) -> Option<&Arc<indices::Definitions>> {
        match &self.package.domain { PackageDomain::SovereigntyIndices(definitions) => Some(definitions), _ => None }
    }
    pub(crate) fn evaluate(&self, rule: Rule, input: &PolicyInput) -> Result<PolicyPlan, CallError> {
        policy::evaluate(&self.registry.lua, &self.package, rule, input).map_err(db_err)
    }

    pub(crate) fn run_adaption<T: mlua::UserData + Send + Sync + 'static>(&self, operation: &str, input: T, deadline: std::time::Instant) -> Result<Value, CallError> {
        if self.adaption().is_none() || !["InjectChips", "ChipExchange"].contains(&operation) {
            return Err(db_err(anyhow::anyhow!("unsupported adaptation policy operation")));
        }
        let input = Value::UserData(self.registry.lua.create_userdata(input).map_err(db_err)?);
        run_adaption(&self.registry.lua, &self.package.row.source, &self.package.config, Some(operation), input, Some(deadline)).map_err(db_err)
    }
}

pub(crate) struct OwnedAdmission { policy: PinnedPolicy }
impl OwnedAdmission {
    pub(crate) fn policy(&self) -> &PinnedPolicy { &self.policy }
}

#[derive(Debug)]
pub(crate) enum AdmissionError { Stale }
impl From<AdmissionError> for CallError {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Stale => db_err(anyhow::anyhow!("authored policy is not current")),
        }
    }
}

impl PinnedPolicy {
    pub(super) fn admit(&self, current: &Arc<Registry>) -> Result<OwnedAdmission, AdmissionError> {
        if self.retained || !Arc::ptr_eq(&self.registry, current)
            || !current.content.packages.get(self.namespace()).is_some_and(|p| Arc::ptr_eq(p, &self.package)) {
            return Err(AdmissionError::Stale);
        }
        Ok(OwnedAdmission { policy: Self { registry: self.registry.clone(), package: self.package.clone(), retained: false } })
    }
    /// Pins an already-compiled current package as retained content; it never gains admission.
    pub(super) fn pin_retained(registry: Arc<Registry>, package: Arc<Package>) -> Self {
        Self { registry, package, retained: true }
    }
    /// The pinned package, so the publisher can rebind this pin to a newer registry.
    pub(super) fn package(&self) -> &Arc<Package> { &self.package }
    /// Whether this pin's evaluation context is already the given registry.
    pub(super) fn pinned_on(&self, registry: &Arc<Registry>) -> bool { Arc::ptr_eq(&self.registry, registry) }
    /// Rebuilds a retained package from the catalogue's shared row; the
    /// catalogue's decoded adaptation build is reused, never decoded twice.
    pub(super) fn reopen(registry: Arc<Registry>, row: Arc<crate::dao::lua_content::PackageRow>, adaption: Option<Arc<adaption::Definitions>>, config: &LuaConfig, sde: &crate::sde::Sde) -> Result<Self, CallError> {
        let expected = compiler(&row.namespace).map_err(db_err)?;
        if row.schema_version != 1 || row.compiler_identity != expected {
            return Err(db_err(anyhow::anyhow!("retained authored policy unavailable or incompatible")));
        }
        let package = build_package(&registry.lua, row, config, sde, daily_goals::model::ValidationContext::Retained, adaption).map_err(db_err)?;
        Ok(Self { registry, package: Arc::new(package), retained: true })
    }
}

fn compiler(namespace: &str) -> anyhow::Result<&'static str> {
    match namespace { "sovereignty" => Ok(COMPILER_IDENTITY), "ship_adaption" => Ok(ADAPTION_COMPILER_IDENTITY), "daily_goals" => Ok(DAILY_GOALS_COMPILER_IDENTITY), indices::NAMESPACE => Ok(INDICES_COMPILER_IDENTITY), _ => anyhow::bail!("unsupported content namespace") }
}

fn messages(lua: &Lua, source: &[u8], config: &LuaConfig) -> mlua::Result<std::collections::BTreeMap<String, String>> {
    let source = std::str::from_utf8(source).map_err(mlua::Error::external)?;
    let value = policy::run_closed(lua, source, config, (), None)?;
    let Value::Table(table) = value else { return Err(mlua::Error::runtime("content messages must be a table")); };
    let mut result = std::collections::BTreeMap::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let (Value::String(key), Value::String(value)) = (key, value) else { return Err(mlua::Error::runtime("content messages must be strings")); };
        let key = key.to_str()?.to_owned();
        let value = value.to_str()?.to_owned();
        if key.is_empty() || key.len() > MESSAGE_CATALOGUE_LIMIT || value.is_empty() || value.len() > config.data_file_limit_bytes || result.len() >= MESSAGE_CATALOGUE_LIMIT {
            return Err(mlua::Error::runtime("invalid content message"));
        }
        result.insert(key, value);
    }
    Ok(result)
}

fn run_adaption(lua: &Lua, source: &[u8], config: &LuaConfig, operation: Option<&str>, input: Value, deadline: Option<std::time::Instant>) -> mlua::Result<Value> {
    let source = std::str::from_utf8(source).map_err(mlua::Error::external)?;
    let wrapper = format!(r#"local operation, input = ...
        local exports = (function()
{source}
        end)()
        if type(exports) ~= 'table' or type(exports.InjectChips) ~= 'function' or type(exports.ChipExchange) ~= 'function' then error('invalid adaptation exports') end
        for key in pairs(exports) do if key ~= 'InjectChips' and key ~= 'ChipExchange' then error('unknown adaptation export') end end
        if operation == nil then return true end
        return exports[operation](input)
    "#);
    policy::run_closed(lua, &wrapper, config, (operation, input), deadline)
}

fn fingerprint(compiler: &str, namespace: &str, version: &str, registration: &[u8], source: &[u8], data: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in [compiler.as_bytes(), namespace.as_bytes(), version.as_bytes(), registration, source, data] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

/// Rebuilds a retained catalogue without a running Lua host or deployment files.
pub(crate) fn decode_retained_adaption(row: &crate::dao::lua_content::PackageRow, config: &LuaConfig, sde: &crate::sde::Sde) -> anyhow::Result<adaption::Definitions> {
    anyhow::ensure!(row.namespace == "ship_adaption" && row.schema_version == 1 && row.compiler_identity == ADAPTION_COMPILER_IDENTITY, "incompatible retained adaptation package");
    anyhow::ensure!(fingerprint(&row.compiler_identity, &row.namespace, &row.version, &row.registration, &row.source, &row.data).as_slice() == row.fingerprint.as_slice(), "retained package digest mismatch");
    let maximum = config.data_file_limit_bytes.checked_mul(RETAINED_SIZE_FACTOR).ok_or_else(|| anyhow::anyhow!("invalid content limit"))?;
    anyhow::ensure!(row.data.len() <= maximum, "adaptation package exceeds size limit");
    let mut retained: AdaptionData = serde_json::from_slice(&row.data)?;
    anyhow::ensure!(retained.messages_source.len() <= config.data_file_limit_bytes && retained.messages.len() <= MESSAGE_CATALOGUE_LIMIT && retained.messages.iter().all(|(k, v)| !k.is_empty() && k.len() <= MESSAGE_CATALOGUE_LIMIT && !v.is_empty() && v.len() <= config.data_file_limit_bytes), "invalid retained messages");
    retained.bundle.validate(config.data_file_limit_bytes)?;
    anyhow::ensure!(row.version.ends_with(&format!("/{}", retained.bundle.manifest_hash())), "adaptation version digest mismatch");
    let rebuilt = adaption::build(&retained.bundle, sde, retained.messages)?;
    anyhow::ensure!(rebuilt == retained.catalogue, "retained catalogue does not match bundle");
    Ok(rebuilt)
}

/// Canonical stored identity for one package, with the compiler identity and
/// content digest stamped into the row the package shares with the catalogue.
pub(super) fn package_row(namespace: String, version: String, registration: Vec<u8>, source: Vec<u8>, data: Vec<u8>) -> mlua::Result<Arc<crate::dao::lua_content::PackageRow>> {
    let compiler = compiler(&namespace).map_err(|e| mlua::Error::runtime(e.to_string()))?;
    let fingerprint = fingerprint(compiler, &namespace, &version, &registration, &source, &data);
    Ok(Arc::new(crate::dao::lua_content::PackageRow { namespace, version, schema_version: 1, compiler_identity: compiler.to_owned(), registration, source, data, fingerprint: fingerprint.to_vec() }))
}

/// Rust-only validation of a stored row when the VM-free build does not apply:
/// compiler identity, schema, size bounds and the recorded content digest.
pub(super) fn verify_row(row: &crate::dao::lua_content::PackageRow, data_file_limit_bytes: usize) -> anyhow::Result<()> {
    let compiler = compiler(&row.namespace)?;
    anyhow::ensure!(row.schema_version == 1 && row.compiler_identity == compiler, "incompatible retained package");
    anyhow::ensure!(row.fingerprint.len() == 32, "invalid retained package fingerprint");
    anyhow::ensure!(!row.version.is_empty() && row.registration.len() <= data_file_limit_bytes && row.source.len() <= data_file_limit_bytes, "invalid authored package identity or size");
    let retained = matches!(row.namespace.as_str(), "ship_adaption" | "sovereignty_indices");
    let maximum = data_file_limit_bytes.checked_mul(if retained { RETAINED_SIZE_FACTOR } else { 1 }).ok_or_else(|| anyhow::anyhow!("invalid content limit"))?;
    anyhow::ensure!(row.data.len() <= maximum, "retained package exceeds size limit");
    anyhow::ensure!(fingerprint(compiler, &row.namespace, &row.version, &row.registration, &row.source, &row.data).as_slice() == row.fingerprint.as_slice(), "retained package digest mismatch");
    Ok(())
}

fn build_package(lua: &Lua, row: Arc<crate::dao::lua_content::PackageRow>, config: &LuaConfig, sde: &crate::sde::Sde, daily_validation: daily_goals::model::ValidationContext<'_>, retained_adaption: Option<Arc<adaption::Definitions>>) -> mlua::Result<Package> {
    let compiler = compiler(&row.namespace).map_err(|e| mlua::Error::runtime(e.to_string()))?;
    if row.version.is_empty() || [row.registration.len(), row.source.len()].into_iter().any(|n| n > config.data_file_limit_bytes) {
        return Err(mlua::Error::runtime("invalid authored package identity or size"));
    }
    let domain = if row.namespace == "sovereignty" {
        if row.data.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("policy data exceeds size limit")); }
        PackageDomain::Sovereignty(policy::Values::parse(&row.data, &row.version)?)
    } else if row.namespace == "daily_goals" {
        if row.data.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("daily data exceeds size limit")); }
        let definitions = daily_goals::model::Definitions::parse(&row.data, &row.version, daily_validation).map_err(|error| mlua::Error::runtime(error.to_string()))?;
        daily_goals::plans::validate(lua, &row.source, config)?;
        PackageDomain::DailyGoals(Arc::new(definitions))
    } else if row.namespace == indices::NAMESPACE {
        let maximum = config.data_file_limit_bytes.checked_mul(RETAINED_SIZE_FACTOR).ok_or_else(|| mlua::Error::runtime("invalid content limit"))?;
        if row.data.len() > maximum { return Err(mlua::Error::runtime("indices package exceeds size limit")); }
        let sde = match daily_validation { daily_goals::model::ValidationContext::Activation(sde) => Some(sde), daily_goals::model::ValidationContext::Retained => None };
        PackageDomain::SovereigntyIndices(Arc::new(indices::rebuild(lua, &row.source, &row.data, &row.version, config, sde)?))
    } else {
        let maximum = config.data_file_limit_bytes.checked_mul(RETAINED_SIZE_FACTOR).ok_or_else(|| mlua::Error::runtime("invalid content limit"))?;
        if row.data.len() > maximum { return Err(mlua::Error::runtime("adaptation package exceeds size limit")); }
        let mut retained: AdaptionData = serde_json::from_slice(&row.data).map_err(mlua::Error::external)?;
        if retained.messages_source.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("message source exceeds size limit")); }
        retained.bundle.validate(config.data_file_limit_bytes).map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if !row.version.ends_with(&format!("/{}", retained.bundle.manifest_hash())) { return Err(mlua::Error::runtime("adaptation version digest mismatch")); }
        protocol::validate().map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if !matches!(run_adaption(lua, &row.source, config, None, Value::Nil, None)?, Value::Boolean(true)) { return Err(mlua::Error::runtime("invalid adaptation policy")); }
        let texts = messages(lua, &retained.messages_source, config)?;
        if texts != retained.messages { return Err(mlua::Error::runtime("captured messages do not match source")); }
        let definitions = match retained_adaption {
            Some(definitions) => definitions,
            None => {
                let rebuilt = adaption::build(&retained.bundle, sde, texts).map_err(|e| mlua::Error::runtime(e.to_string()))?;
                if rebuilt != retained.catalogue { return Err(mlua::Error::runtime("retained catalogue does not match bundle")); }
                Arc::new(rebuilt)
            }
        };
        PackageDomain::Adaption(definitions)
    };
    if fingerprint(compiler, &row.namespace, &row.version, &row.registration, &row.source, &row.data).as_slice() != row.fingerprint.as_slice() {
        return Err(mlua::Error::runtime("retained authored policy digest mismatch"));
    }
    let package = Package { row, config: config.clone(), domain };
    if matches!(&package.domain, PackageDomain::Sovereignty(_)) { policy::validate(lua, &package)?; }
    Ok(package)
}

pub(super) fn bytes(root: &Path, name: &str, limit: usize) -> mlua::Result<Vec<u8>> {
    use std::io::Read;
    let path = root.join(name).canonicalize().map_err(mlua::Error::external)?;
    if !path.starts_with(root) { return Err(mlua::Error::runtime("content path escapes root")); }
    let file = std::fs::File::open(path).map_err(mlua::Error::external)?;
    let maximum = u64::try_from(limit).ok().and_then(|v| v.checked_add(1)).ok_or_else(|| mlua::Error::runtime("invalid content size limit"))?;
    let mut data = Vec::new();
    file.take(maximum).read_to_end(&mut data).map_err(mlua::Error::external)?;
    if data.len() > limit { return Err(mlua::Error::runtime("content file exceeds size limit")); }
    Ok(data)
}

fn empty(table: Table) -> mlua::Result<bool> { Ok(table.pairs::<Value, Value>().next().is_none()) }

fn string(table: &Table, key: &str) -> mlua::Result<String> {
    match table.get::<Value>(key)? {
        Value::String(value) => Ok(value.to_str()?.to_owned()),
        _ => Err(mlua::Error::runtime(format!("content field {key} must be a string"))),
    }
}

struct DatasetHandle { bundle: Arc<bundle::Bundle>, name: String }
impl mlua::UserData for DatasetHandle {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("name", |_, this| Ok(this.name.clone()));
        fields.add_field_method_get("records", |_, this| {
            Ok(this.bundle.records(&this.name).map_err(|e| mlua::Error::runtime(e.to_string()))?.len())
        });
    }
}

pub(super) fn install_data(lua: &Lua, env: &Table, bundle: Option<Arc<bundle::Bundle>>, open: Arc<AtomicBool>) -> mlua::Result<()> {
    let content = lua.create_table()?;
    content.set("data", lua.create_function(move |lua, name: String| {
        if !open.load(Ordering::Acquire) { return Err(mlua::Error::runtime("content data is only available during loading")); }
        if !bundle::known_family(&name) { return Err(mlua::Error::runtime("unknown content dataset")); }
        bundle.as_ref().filter(|bundle| bundle.has_family(&name)).map(|bundle| lua.create_userdata(DatasetHandle { bundle: bundle.clone(), name })).transpose()
    })?)?;
    env.set("content", content)
}

/// Shared canonical identity handed to a generation build: the stored row, the
/// decoded adaptation definitions and, once the entry is pinned, the already
/// built package. The flat map holds no catalogue back-reference.
pub(super) struct CanonicalPackage {
    pub(super) row: Arc<crate::dao::lua_content::PackageRow>,
    pub(super) package: Option<Arc<Package>>,
    pub(super) adaption: Option<Arc<adaption::Definitions>>,
}
pub(super) type CanonicalPackages = std::collections::HashMap<(String, String), CanonicalPackage>;

/// The already-built canonical package when `row` is the complete stored
/// identity, byte for byte. Contract-required activation-phase reference
/// checks rerun on the retained definitions; nothing is rebuilt.
fn reuse(canonical: &CanonicalPackages, row: &crate::dao::lua_content::PackageRow, sde: &crate::sde::Sde) -> mlua::Result<Option<Arc<Package>>> {
    let Some(hit) = canonical.get(&(row.namespace.clone(), row.version.clone())) else { return Ok(None) };
    if hit.row.as_ref() != row { return Ok(None) }
    let Some(package) = &hit.package else { return Ok(None) };
    match &package.domain {
        PackageDomain::DailyGoals(definitions) => definitions.validate(daily_goals::model::ValidationContext::Activation(sde)).map_err(|error| mlua::Error::runtime(error.to_string()))?,
        PackageDomain::SovereigntyIndices(definitions) => indices::validate(definitions, Some(sde))?,
        _ => {}
    }
    Ok(Some(package.clone()))
}

/// Swaps a freshly built package's row for the canonical shared Arc when the
/// stored identity matches byte-for-byte, so one package identity owns one row.
fn share_row(canonical: &CanonicalPackages, mut package: Package) -> Package {
    if let Some(hit) = canonical.get(&(package.namespace().to_owned(), package.version().to_owned())) {
        if hit.row.as_ref() == package.row.as_ref() { package.row = hit.row.clone(); }
    }
    package
}

pub(super) fn register(lua: &Lua, register: &Table, definitions: Arc<Mutex<ContentDefinitions>>, open: Arc<AtomicBool>, root: &Path, script: &str, registration: Vec<u8>, config: &LuaConfig, bundle: Option<Arc<bundle::Bundle>>, sde: Arc<crate::sde::Sde>, canonical: Arc<CanonicalPackages>) -> mlua::Result<()> {
    let root = root.to_owned();
    let script = script.to_owned();
    let config = config.clone();
    register.set("content", lua.create_function(move |lua, spec: Table| {
        if !open.load(Ordering::Acquire) { return Err(mlua::Error::runtime("registration is closed")); }
        for pair in spec.clone().pairs::<String, Value>() {
            let (key, _) = pair?;
            if !["key", "schema_version", "content_version", "datasets", "state_schema", "operations", "progress_rules"].contains(&key.as_str()) {
                return Err(mlua::Error::runtime("unknown content registration field"));
            }
        }
        let namespace = string(&spec, "key")?;
        let version = string(&spec, "content_version")?;
        let sovereignty = script == "sovereignty_content" && namespace == "sovereignty" && string(&spec, "state_schema")? == "sovereignty_policy_v1";
        let adaption = script == "ship_adaption_mgr" && namespace == "ship_adaption" && string(&spec, "state_schema")? == "ship_adaption_chip_v1";
        let daily = script == "daily_goals" && namespace == "daily_goals" && string(&spec, "state_schema")? == "daily_goals_v1";
        let indices = script == "sovereignty_indices" && namespace == indices::NAMESPACE && string(&spec, "state_schema")? == "sovereignty_indices_v1";
        if !(sovereignty || adaption || daily || indices) || !matches!(spec.get::<Value>("schema_version")?, Value::Integer(1)) || version.is_empty() {
            return Err(mlua::Error::runtime("unsupported content identity or schema"));
        }
        if !empty(spec.get("progress_rules")?)? { return Err(mlua::Error::runtime("content progress rules must be empty")); }
        let datasets: Table = spec.get("datasets")?;
        if sovereignty || daily {
            if !empty(datasets)? { return Err(mlua::Error::runtime("authored content datasets must be empty")); }
        } else if indices {
            let mut names = Vec::new();
            for pair in datasets.pairs::<Value, Value>() {
                let (index, value) = pair?;
                let (Value::Integer(index), Value::String(value)) = (index, value) else { return Err(mlua::Error::runtime("invalid dataset list")); };
                if index != 1 { return Err(mlua::Error::runtime("invalid dataset list")); }
                names.push(value.to_str()?.to_string());
            }
            if names != [bundle::INDICES_FAMILY] { return Err(mlua::Error::runtime("invalid dataset list")); }
        } else {
            let mut names = std::collections::BTreeSet::new();
            let mut count = 0;
            for pair in datasets.pairs::<Value, Value>() {
                let (index, value) = pair?;
                let Value::Integer(index) = index else { return Err(mlua::Error::runtime("invalid dataset list")); };
                let Value::String(value) = value else { return Err(mlua::Error::runtime("invalid dataset name")); };
                if index < 1 || index > bundle::FAMILIES.len() as i64 || !names.insert(value.to_str()?.to_owned()) { return Err(mlua::Error::runtime("invalid dataset list")); }
                count += 1;
            }
            if count != bundle::FAMILIES.len() || !bundle::FAMILIES.iter().all(|name| names.contains(*name)) { return Err(mlua::Error::runtime("missing content dataset")); }
        }
        let operations: Table = spec.get("operations")?;
        let expected: &[&str] = if sovereignty { &["InitializeCapital", "CapitalLost", "SelfDestruct"] } else if daily { &["Assign", "Match", "Cascade", "Project"] } else if indices { &["Compile"] } else { &["InjectChips", "ChipExchange"] };
        let mut count = 0;
        for pair in operations.pairs::<Value, Value>() {
            let (index, name) = pair?;
            let Value::Integer(index) = index else { return Err(mlua::Error::runtime("invalid operation list")); };
            let Value::String(name) = name else { return Err(mlua::Error::runtime("invalid operation name")); };
            let slot = usize::try_from(index).ok().and_then(|i| i.checked_sub(1)).and_then(|i| expected.get(i));
            let name = name.to_str()?.to_owned();
            if !matches!(slot, Some(value) if *value == name.as_str()) { return Err(mlua::Error::runtime("unsupported content operations")); }
            count += 1;
        }
        if count != expected.len() { return Err(mlua::Error::runtime("missing content operation")); }
        let package: Arc<Package> = if sovereignty {
            let source = bytes(&root, "content/sovereignty/policy.lua", config.data_file_limit_bytes)?;
            let data = bytes(&root, "content/sovereignty/data/policy.json", config.data_file_limit_bytes)?;
            let row = package_row(namespace.clone(), version, registration.clone(), source, data)?;
            match reuse(&canonical, &row, &sde)? {
                Some(package) => package,
                None => Arc::new(share_row(&canonical, build_package(lua, row, &config, &sde, daily_goals::model::ValidationContext::Activation(&sde), None)?)),
            }
        } else if daily {
            let source = bytes(&root, "content/daily_goals/policy.lua", config.data_file_limit_bytes)?;
            let data = bytes(&root, "content/daily_goals/data/daily_goals.json", config.data_file_limit_bytes)?;
            let row = package_row(namespace.clone(), version, registration.clone(), source, data)?;
            match reuse(&canonical, &row, &sde)? {
                Some(package) => package,
                None => Arc::new(share_row(&canonical, build_package(lua, row, &config, &sde, daily_goals::model::ValidationContext::Activation(&sde), None)?)),
            }
        } else if indices {
            let bundle = bundle.as_ref().ok_or_else(|| mlua::Error::runtime("indices bundle is not configured"))?;
            if !bundle.has_family(bundle::INDICES_FAMILY) { return Err(mlua::Error::runtime("indices dataset is not published")); }
            let dataset = bundle.files.get(bundle::INDICES_FAMILY).cloned().ok_or_else(|| mlua::Error::runtime("indices dataset is not published"))?;
            let source = bytes(&root, "content/sovereignty_indices/policy.lua", config.data_file_limit_bytes)?;
            let authored = bytes(&root, "content/sovereignty_indices/data/policy.json", config.data_file_limit_bytes)?;
            if indices::authored_version(&authored)? != version { return Err(mlua::Error::runtime("indices policy version mismatch")); }
            let version = format!("{version}/{}", sha256_hex(&dataset));
            let key = (namespace.clone(), version.clone());
            let build = |compiled: Arc<indices::Definitions>| -> mlua::Result<Package> {
                let data = indices::retained_data(&authored, &dataset, &compiled)?;
                let maximum = config.data_file_limit_bytes.checked_mul(RETAINED_SIZE_FACTOR).ok_or_else(|| mlua::Error::runtime("invalid content limit"))?;
                if data.len() > maximum || registration.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("indices package exceeds size limit")); }
                Ok(Package { row: package_row(namespace.clone(), version.clone(), registration.clone(), source.clone(), data)?, config: config.clone(), domain: PackageDomain::SovereigntyIndices(compiled) })
            };
            let compile = || indices::compile(lua, &source, &authored, &dataset, &config, Some(sde.as_ref())).map(Arc::new);
            match canonical.get(&key).and_then(|hit| hit.package.as_ref().and_then(|package| match &package.domain { PackageDomain::SovereigntyIndices(definitions) => Some(definitions.clone()), _ => None })) {
                Some(compiled) => match reuse(&canonical, &build(compiled)?.row, &sde)? {
                    Some(package) => package,
                    None => Arc::new(build(compile()?)?),
                },
                None => Arc::new(build(compile()?)?),
            }
        } else {
            let bundle = bundle.as_ref().ok_or_else(|| mlua::Error::runtime("adaptation bundle is not configured"))?;
            let source = bytes(&root, "content/ship_adaption/policy.lua", config.data_file_limit_bytes)?;
            let messages_source = bytes(&root, "content/ship_adaption/messages.lua", config.data_file_limit_bytes)?;
            protocol::validate().map_err(|e| mlua::Error::runtime(e.to_string()))?;
            if !matches!(run_adaption(lua, &source, &config, None, Value::Nil, None)?, Value::Boolean(true)) { return Err(mlua::Error::runtime("invalid adaptation policy")); }
            let texts = messages(lua, &messages_source, &config)?;
            let version = format!("{version}/{}", bundle.manifest_hash());
            let key = (namespace.clone(), version.clone());
            let build = |catalogue: Arc<adaption::Definitions>| -> mlua::Result<Package> {
                let data = serde_json::to_vec(&AdaptionDataRef { bundle, messages_source: &messages_source, messages: &texts, catalogue: catalogue.as_ref() }).map_err(mlua::Error::external)?;
                let maximum = config.data_file_limit_bytes.checked_mul(RETAINED_SIZE_FACTOR).ok_or_else(|| mlua::Error::runtime("invalid content limit"))?;
                if data.len() > maximum || registration.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("adaptation package exceeds size limit")); }
                Ok(Package { row: package_row(namespace.clone(), version.clone(), registration.clone(), source.clone(), data)?, config: config.clone(), domain: PackageDomain::Adaption(catalogue) })
            };
            let rebuild = || adaption::build(bundle, &sde, texts.clone()).map(Arc::new).map_err(|e| mlua::Error::runtime(e.to_string()));
            match canonical.get(&key).and_then(|hit| hit.adaption.clone().or_else(|| hit.package.as_ref().and_then(|package| package.adaption().cloned()))) {
                Some(definitions) => {
                    let candidate = build(definitions)?;
                    match reuse(&canonical, &candidate.row, &sde)? {
                        Some(package) => package,
                        None if canonical.get(&key).is_some_and(|hit| hit.row.as_ref() == candidate.row.as_ref()) => Arc::new(share_row(&canonical, candidate)),
                        None => Arc::new(build(rebuild()?)?),
                    }
                }
                None => Arc::new(build(rebuild()?)?),
            }
        };
        let mut definitions = crate::poison::lock_or_exit(definitions.lock());
        if definitions.packages.contains_key(&namespace) { return Err(mlua::Error::runtime("duplicate content namespace")); }
        definitions.packages.insert(namespace, package);
        Ok(())
    })?)
}

#[cfg(all(test, feature = "tests-lua"))]
mod tests;
