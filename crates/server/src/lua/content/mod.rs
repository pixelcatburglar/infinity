//! Immutable sovereignty, adaptation and daily content captured per Lua generation.
//! Adaptation data comes from a validated deployment bundle and daily data is authored JSON;
//! retained daily packages preserve references validated at activation across SDE changes.
//! Gateway handlers, progress settlement, node and mission gameplay are not implemented here.

pub(crate) mod adaption;
pub(super) mod bundle;
pub(crate) mod daily_goals;
mod policy;
pub(crate) mod protocol;

use std::{collections::HashMap, path::Path, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};
use mlua::{Lua, Table, Value};
use sha2::{Digest, Sha256};
use crate::{config::lua::LuaConfig, services::{CallError, db_err}};
use super::registry::Registry;

pub(crate) use policy::{PolicyInput, PolicyPlan, Rule};

/// Server-defined identity of the closed sovereignty policy compiler.
const COMPILER_IDENTITY: &str = "sovereignty-policy-v1";
/// Server-defined identity of the adaptation catalogue compiler.
const ADAPTION_COMPILER_IDENTITY: &str = "ship-adaption-chip-v1";
/// Server-defined identity of the closed daily-goal policy compiler.
const DAILY_GOALS_COMPILER_IDENTITY: &str = "daily-goals-policy-v1";
/// Server safety ceiling on message keys and the number of catalogue entries.
const MESSAGE_CATALOGUE_LIMIT: usize = 256;
/// JSON integer-array representation overhead for the seven bounded files and metadata.
const RETAINED_SIZE_FACTOR: usize = 40;

enum PackageDomain {
    Sovereignty(policy::Values),
    Adaption { definitions: Arc<adaption::Definitions>, bundle: Arc<bundle::Bundle> },
    DailyGoals(Arc<daily_goals::model::Definitions>),
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
        let (PackageDomain::Adaption { definitions: old, .. }, PackageDomain::Adaption { definitions: new, .. }) = (&old.domain, &new.domain) else {
            anyhow::bail!("invalid adaptation catalogue domain");
        };
        anyhow::ensure!(new.compatible(old), "adaptation catalogue removed a chip currency");
    }
    Ok(())
}

pub(super) struct Package {
    pub namespace: String,
    pub version: String,
    pub fingerprint: [u8; 32],
    pub registration: Vec<u8>,
    pub source: Vec<u8>,
    pub data: Vec<u8>,
    pub config: LuaConfig,
    domain: PackageDomain,
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
    pub(crate) fn namespace(&self) -> &str { &self.package.namespace }
    pub(crate) fn same_package(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.registry, &other.registry) && Arc::ptr_eq(&self.package, &other.package)
    }
    pub(crate) fn version(&self) -> &str { &self.package.version }
    pub(crate) fn fingerprint(&self) -> &[u8; 32] { &self.package.fingerprint }
    pub(crate) fn checkpoint_interval_ms(&self) -> Result<u64, CallError> {
        match &self.package.domain {
            PackageDomain::Sovereignty(values) => Ok(values.checkpoint_ms),
            _ => Err(db_err(anyhow::anyhow!("checkpoint interval requires sovereignty policy"))),
        }
    }
    pub(crate) fn adaption(&self) -> Option<&Arc<adaption::Definitions>> {
        match &self.package.domain { PackageDomain::Adaption { definitions, bundle } => { let _ = bundle; Some(definitions) }, _ => None }
    }
    pub(crate) fn daily_goals(&self) -> Option<&Arc<daily_goals::model::Definitions>> {
        match &self.package.domain { PackageDomain::DailyGoals(definitions) => Some(definitions), _ => None }
    }
    pub(crate) fn evaluate(&self, rule: Rule, input: &PolicyInput) -> Result<PolicyPlan, CallError> {
        policy::evaluate(&self.registry.lua, &self.package, rule, input).map_err(db_err)
    }

    pub(crate) fn run_adaption<T: mlua::UserData + Send + Sync + 'static>(&self, operation: &str, input: T, deadline: std::time::Instant) -> Result<Value, CallError> {
        if self.adaption().is_none() || !["InjectChips", "ChipExchange"].contains(&operation) {
            return Err(db_err(anyhow::anyhow!("unsupported adaptation policy operation")));
        }
        let input = Value::UserData(self.registry.lua.create_userdata(input).map_err(db_err)?);
        run_adaption(&self.registry.lua, &self.package.source, &self.package.config, Some(operation), input, Some(deadline)).map_err(db_err)
    }
}

pub(crate) struct OwnedAdmission { policy: PinnedPolicy }
impl OwnedAdmission {
    pub(crate) fn policy(&self) -> &PinnedPolicy { &self.policy }
}

#[derive(Debug)]
pub(crate) enum AdmissionError { Stale, Infrastructure(CallError) }
impl From<AdmissionError> for CallError {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Stale => db_err(anyhow::anyhow!("authored policy is not current")),
            AdmissionError::Infrastructure(error) => error,
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
    pub(super) fn reopen(registry: Arc<Registry>, row: crate::dao::lua_content::PackageRow, config: &LuaConfig, sde: &crate::sde::Sde) -> Result<Self, CallError> {
        let expected = compiler(&row.namespace).map_err(db_err)?;
        if (row.namespace == "sovereignty" && !registry.content.packages.contains_key(&row.namespace)) || row.schema_version != 1 || row.compiler_identity != expected {
            return Err(db_err(anyhow::anyhow!("retained authored policy unavailable or incompatible")));
        }
        let package = build_package(&registry.lua, row.namespace, row.version, row.registration, row.source, row.data, config, sde, daily_goals::model::ValidationContext::Retained).map_err(db_err)?;
        if package.fingerprint.as_slice() != row.fingerprint.as_slice() {
            return Err(db_err(anyhow::anyhow!("retained authored policy digest mismatch")));
        }
        Ok(Self { registry, package: Arc::new(package), retained: true })
    }
}

impl Package {
    pub(super) fn row(&self) -> crate::dao::lua_content::PackageRow {
        crate::dao::lua_content::PackageRow { namespace: self.namespace.clone(), version: self.version.clone(), schema_version: 1, compiler_identity: compiler(&self.namespace).expect("validated namespace").into(), registration: self.registration.clone(), source: self.source.clone(), data: self.data.clone(), fingerprint: self.fingerprint.to_vec() }
    }
}

fn compiler(namespace: &str) -> anyhow::Result<&'static str> {
    match namespace { "sovereignty" => Ok(COMPILER_IDENTITY), "ship_adaption" => Ok(ADAPTION_COMPILER_IDENTITY), "daily_goals" => Ok(DAILY_GOALS_COMPILER_IDENTITY), _ => anyhow::bail!("unsupported content namespace") }
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

fn build_package(lua: &Lua, namespace: String, version: String, registration: Vec<u8>, source: Vec<u8>, data: Vec<u8>, config: &LuaConfig, sde: &crate::sde::Sde, daily_validation: daily_goals::model::ValidationContext<'_>) -> mlua::Result<Package> {
    let compiler = compiler(&namespace).map_err(|e| mlua::Error::runtime(e.to_string()))?;
    if version.is_empty() || [registration.len(), source.len()].into_iter().any(|n| n > config.data_file_limit_bytes) {
        return Err(mlua::Error::runtime("invalid authored package identity or size"));
    }
    let domain = if namespace == "sovereignty" {
        if data.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("policy data exceeds size limit")); }
        PackageDomain::Sovereignty(policy::Values::parse(&data, &version)?)
    } else if namespace == "daily_goals" {
        if data.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("daily data exceeds size limit")); }
        let definitions = daily_goals::model::Definitions::parse(&data, &version, daily_validation).map_err(|error| mlua::Error::runtime(error.to_string()))?;
        daily_goals::plans::validate(lua, &source, config)?;
        PackageDomain::DailyGoals(Arc::new(definitions))
    } else {
        let maximum = config.data_file_limit_bytes.checked_mul(RETAINED_SIZE_FACTOR).ok_or_else(|| mlua::Error::runtime("invalid content limit"))?;
        if data.len() > maximum { return Err(mlua::Error::runtime("adaptation package exceeds size limit")); }
        let mut retained: AdaptionData = serde_json::from_slice(&data).map_err(mlua::Error::external)?;
        if retained.messages_source.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("message source exceeds size limit")); }
        retained.bundle.validate(config.data_file_limit_bytes).map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if !version.ends_with(&format!("/{}", retained.bundle.manifest_hash())) { return Err(mlua::Error::runtime("adaptation version digest mismatch")); }
        protocol::validate().map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if !matches!(run_adaption(lua, &source, config, None, Value::Nil, None)?, Value::Boolean(true)) { return Err(mlua::Error::runtime("invalid adaptation policy")); }
        let texts = messages(lua, &retained.messages_source, config)?;
        if texts != retained.messages { return Err(mlua::Error::runtime("captured messages do not match source")); }
        let rebuilt = adaption::build(&retained.bundle, sde, texts).map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if rebuilt != retained.catalogue { return Err(mlua::Error::runtime("retained catalogue does not match bundle")); }
        PackageDomain::Adaption { definitions: Arc::new(rebuilt), bundle: Arc::new(retained.bundle) }
    };
    let fingerprint = fingerprint(compiler, &namespace, &version, &registration, &source, &data);
    let package = Package { namespace, version, fingerprint, registration, source, data, config: config.clone(), domain };
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
        if !bundle::FAMILIES.contains(&name.as_str()) { return Err(mlua::Error::runtime("unknown content dataset")); }
        bundle.as_ref().map(|bundle| lua.create_userdata(DatasetHandle { bundle: bundle.clone(), name })).transpose()
    })?)?;
    env.set("content", content)
}

pub(super) fn register(lua: &Lua, register: &Table, definitions: Arc<Mutex<ContentDefinitions>>, open: Arc<AtomicBool>, root: &Path, script: &str, registration: Vec<u8>, config: &LuaConfig, bundle: Option<Arc<bundle::Bundle>>, sde: Arc<crate::sde::Sde>) -> mlua::Result<()> {
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
        if !(sovereignty || adaption || daily) || !matches!(spec.get::<Value>("schema_version")?, Value::Integer(1)) || version.is_empty() {
            return Err(mlua::Error::runtime("unsupported content identity or schema"));
        }
        if !empty(spec.get("progress_rules")?)? { return Err(mlua::Error::runtime("content progress rules must be empty")); }
        let datasets: Table = spec.get("datasets")?;
        if sovereignty || daily {
            if !empty(datasets)? { return Err(mlua::Error::runtime("authored content datasets must be empty")); }
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
        let expected: &[&str] = if sovereignty { &["InitializeCapital", "CapitalLost", "SelfDestruct"] } else if daily { &["Assign", "Match", "Cascade", "Project"] } else { &["InjectChips", "ChipExchange"] };
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
        let package = if sovereignty {
            let source = bytes(&root, "content/sovereignty/policy.lua", config.data_file_limit_bytes)?;
            let data = bytes(&root, "content/sovereignty/data/policy.json", config.data_file_limit_bytes)?;
            build_package(lua, namespace.clone(), version, registration.clone(), source, data, &config, &sde, daily_goals::model::ValidationContext::Activation(&sde))?
        } else if daily {
            let source = bytes(&root, "content/daily_goals/policy.lua", config.data_file_limit_bytes)?;
            let data = bytes(&root, "content/daily_goals/data/daily_goals.json", config.data_file_limit_bytes)?;
            build_package(lua, namespace.clone(), version, registration.clone(), source, data, &config, &sde, daily_goals::model::ValidationContext::Activation(&sde))?
        } else {
            let bundle = bundle.as_ref().ok_or_else(|| mlua::Error::runtime("adaptation bundle is not configured"))?;
            let source = bytes(&root, "content/ship_adaption/policy.lua", config.data_file_limit_bytes)?;
            let messages_source = bytes(&root, "content/ship_adaption/messages.lua", config.data_file_limit_bytes)?;
            protocol::validate().map_err(|e| mlua::Error::runtime(e.to_string()))?;
            if !matches!(run_adaption(lua, &source, &config, None, Value::Nil, None)?, Value::Boolean(true)) { return Err(mlua::Error::runtime("invalid adaptation policy")); }
            let texts = messages(lua, &messages_source, &config)?;
            let catalogue = adaption::build(bundle, &sde, texts.clone()).map_err(|e| mlua::Error::runtime(e.to_string()))?;
            let data = serde_json::to_vec(&AdaptionDataRef { bundle, messages_source: &messages_source, messages: &texts, catalogue: &catalogue }).map_err(mlua::Error::external)?;
            let maximum = config.data_file_limit_bytes.checked_mul(RETAINED_SIZE_FACTOR).ok_or_else(|| mlua::Error::runtime("invalid content limit"))?;
            if data.len() > maximum || registration.len() > config.data_file_limit_bytes { return Err(mlua::Error::runtime("adaptation package exceeds size limit")); }
            let version = format!("{version}/{}", bundle.manifest_hash());
            let fingerprint = fingerprint(ADAPTION_COMPILER_IDENTITY, &namespace, &version, &registration, &source, &data);
            Package { namespace: namespace.clone(), version, fingerprint, registration: registration.clone(), source, data, config: config.clone(), domain: PackageDomain::Adaption { definitions: Arc::new(catalogue), bundle: bundle.clone() } }
        };
        let package = Arc::new(package);
        let mut definitions = definitions.lock().map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if definitions.packages.contains_key(&namespace) { return Err(mlua::Error::runtime("duplicate content namespace")); }
        definitions.packages.insert(namespace, package);
        Ok(())
    })?)
}

#[cfg(test)]
mod tests;
