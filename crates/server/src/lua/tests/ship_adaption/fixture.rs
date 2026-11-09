//! Synthetic publisher bundle and local host for real script integration tests.
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use serde_json::json;
use crate::hash::sha256_hex;
use infinity_core::{consts::{GROUP_ADAPTION_CHIPS, dogma::{ATTR_REFIT_TICKET_TYPE, ATTR_REFIT_TICKET_QUANTITY}}};
use crate::{db::Db, lua::{host::Resources, reload::Host}, sde::Sde};


pub(crate) struct Fixture {
    pub root: PathBuf,
    pub host: Arc<Host>,
    pub chip: i32,
    pub chip2: i32,
    pub chip_unquoted: i32,
    pub injector: i32,
    pub injector2: i32,
    pub sde: Arc<Sde>,
    pub issuer: i64,
    pub station: i64,
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
}

pub(crate) fn inject_plan(pin: &crate::lua::content::PinnedPolicy, sde: &Sde, type_id: i32, quantity: u32) -> crate::services::ship_adaption::policy::PreparedPlan {
    use crate::services::ship_adaption::policy::{self, Input};
    let definitions = pin.adaption().unwrap().clone();
    let credit = policy::conversion(sde, &definitions, type_id, quantity);
    let input = Input { definitions, item_type: Some(type_id), quantity, credit, issuer: 0, chips: Arc::new(vec![]) };
    policy::plan(pin, input, std::time::Instant::now() + std::time::Duration::from_secs(30)).unwrap().unwrap()
}

pub(crate) async fn create(db: &Db, sde: Arc<Sde>, world: Arc<crate::space::World>) -> Fixture {
    let find = |units: f64| sde.types.group_types(GROUP_ADAPTION_CHIPS).iter().find_map(|&id| {
        let ty = sde.types.get(id)?;
        let target = ty.attribute(ATTR_REFIT_TICKET_TYPE)?;
        if ty.attribute(ATTR_REFIT_TICKET_QUANTITY) != Some(units) || !target.is_finite() || target <= 0.0 || target > f64::from(i32::MAX) || target.fract() != 0.0 { return None; }
        let target = target as i32;
        sde.types.get(target).map(|_| (id, target))
    }).expect("test SDE must contain injector and target");
    let (injector, chip) = find(1.0);
    let (injector2, chip2) = find(10.0);
    let chip_unquoted = sde.types.iter().find(|ty| ty.type_id > chip2 && ty.group_id != Some(GROUP_ADAPTION_CHIPS)).unwrap().type_id;
    assert!(chip < chip2 && chip2 < chip_unquoted);
    let issuer = sde.npc_corporations.iter().find(|corp| corp.corporation_id > 0 && u32::try_from(corp.corporation_id).is_ok()).unwrap().corporation_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let root = std::env::temp_dir().join(format!("adaptation-fixture-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let scripts = root.join("scripts");
    let policy = scripts.join("content/ship_adaption");
    let data = root.join("data");
    std::fs::create_dir_all(&policy).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(scripts.join("ship_adaption_mgr.lua"), include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/ship_adaption_mgr.lua"))).unwrap();
    std::fs::write(policy.join("policy.lua"), include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/ship_adaption/policy.lua"))).unwrap();
    std::fs::write(policy.join("messages.lua"), include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/ship_adaption/messages.lua"))).unwrap();
    let tier = json!({"consume_multiplier":[1.0],"base_unlock_items":[]});
    let records = [
        ("shipadaptionnodes", "1", json!({"_key":1,"node_type":0,"unlock_type":0})),
        ("shipadaptiontrees", "1", json!({"_key":1,"tree_nodes":[{"id":1,"node_id":1,"parent_id":-1}],"t1_node_consume":tier,"t2_node_consume":tier,"t3_node_consume":tier})),
        ("shipadaptionnodetypes", "0", json!({"_key":0,"name":"Root"})),
        ("shipadaptionnodeunlocktypes", "0", json!({"_key":0,"name":"test"})),
        ("shipadaptionconfig", "", json!({"factions":[{"faction_id":1,"ship_groups":[{"group_id":1,"tree_id":1}]}]})),
        ("shipadaptionchips", "chip", json!({"_key":chip,"defaultCorpBonus":{"lpBonus":2},"corpsBonus":{}})),
        ("shipadaptionchipexchangeconfig", "", json!({"availableCorpIDs":[issuer]})),
    ];
    let mut files = BTreeMap::new();
    let mut datasets = BTreeMap::new();
    for (family, key, row) in records {
        let key = if key == "chip" { chip.to_string() } else { key.to_owned() };
        let mut rows = vec![json!({"key":key,"data":row})];
        if family == "shipadaptionchips" {
            rows.push(json!({"key":chip2.to_string(),"data":{"_key":chip2,"defaultCorpBonus":{"lpBonus":2},"corpsBonus":{}}}));
        }
        let count = rows.len();
        let bytes = serde_json::to_vec(&json!({"schema":"cfsd-adaptation-dataset","schema_version":1,"client_build":"test","source_resource":"test","records":rows})).unwrap();
        datasets.insert(family, json!({"path":format!("datasets/{family}.json"),"records":count,"sha256":sha256_hex(&bytes)}));
        files.insert(family, bytes);
    }
    let manifest = serde_json::to_vec(&json!({"schema":"infinity-adaption-content","schema_version":1,"client_build":"test","source_manifest_sha256":"0".repeat(64),"constants_sha256":"0".repeat(64),"chip_types":[chip,chip2,chip_unquoted],"datasets":datasets,"provenance":{"source_validation":{},"chip_type_symbols":["synthetic"]},"validation":{"scope":"chip-ledger","node_task_effect_gameplay_enabled":false}})).unwrap();
    let digest = sha256_hex(&manifest);
    let bundle = data.join("test").join(&digest);
    std::fs::create_dir_all(bundle.join("datasets")).unwrap();
    std::fs::write(bundle.join("manifest.json"), manifest).unwrap();
    for (family, bytes) in files { std::fs::write(bundle.join("datasets").join(format!("{family}.json")), bytes).unwrap(); }
    std::fs::write(data.join("current"), format!("test/{digest}\n")).unwrap();
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(db, hub).await.unwrap();
    let resources = Resources { db: db.clone(), sde: sde.clone(), world, pilots, store_value_limit_bytes: 65536 };
    let config = crate::config::lua::LuaConfig { scripts_dir: scripts.to_string_lossy().into_owned(), content_data_dir: Some(data.to_string_lossy().into_owned()), call_budget_ms: 30_000, ..Default::default() };
    let host = Arc::new(Host::new(config, resources).await.unwrap());
    Fixture { root, host, chip, chip2, chip_unquoted, injector, injector2, sde, issuer, station }
}
