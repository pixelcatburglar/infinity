//! Data captured from the retail Serenity server by a client of this build
//! (build 3653293, macho 496), shipped verbatim under `crates/server/retail/`:
//!
//! * `cdn/`: the content CDN's flatbuffers as the client stored them
//!   (`cache/Storage/<name>/data.bin`, version from `version.txt`).
//! * `machonet/`: the client's `CachedMethodCalls` files, one marshal stream
//!   each: `(key, {'lret': result, 'rret': ..., 'version': ...})`.

use marshal::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use tracing::warn;

/// Retail gateway's captured achievements CDN version.
const ACHIEVEMENTS_VERSION: (u32, u32, u32) = (1, 0, 7);
/// Retail gateway's captured titles CDN version.
const TITLES_VERSION: (u32, u32, u32) = (1, 0, 0);
/// Retail gateway's captured map tags CDN version.
const MAPTAGS_VERSION: (u32, u32, u32) = (1, 1366, 0);

/// A content CDN file and the version the retail gateway announced for it.
pub struct CdnFile {
    pub data: &'static [u8],
    pub(crate) version: (u32, u32, u32),
}

pub fn cdn(name: &str) -> Option<CdnFile> {
    let (data, version): (&'static [u8], _) = match name {
        "achievements" => (include_bytes!("../../server/retail/cdn/achievements.bin"), ACHIEVEMENTS_VERSION),
        "titles" => (include_bytes!("../../server/retail/cdn/titles.bin"), TITLES_VERSION),
        "maptags" => (include_bytes!("../../server/retail/cdn/maptags.bin"), MAPTAGS_VERSION),
        _ => return None,
    };
    Some(CdnFile { data, version })
}

/// Cached method call files, by `service.method[.arg]`.
const CALLS: &[(&str, &[u8])] = &[
    ("config.GetAverageMarketPrices", include_bytes!("../../server/retail/machonet/config.GetAverageMarketPrices.cache")),
    ("config.GetBlackListedPlanets", include_bytes!("../../server/retail/machonet/config.GetBlackListedPlanets.cache")),
    ("config.GetOldStationData", include_bytes!("../../server/retail/machonet/config.GetOldStationData.cache")),
    ("agentMgr.GetAgents", include_bytes!("../../server/retail/machonet/agentMgr.GetAgents.cache")),
    ("standingMgr.GetNPCNPCStandings", include_bytes!("../../server/retail/machonet/standingMgr.GetNPCNPCStandings.cache")),
    ("beyonce.GetFormations", include_bytes!("../../server/retail/machonet/beyonce.GetFormations.cache")),
    ("map.GetSecurityModifiedSystems", include_bytes!("../../server/retail/machonet/map.GetSecurityModifiedSystems.cache")),
    ("bannedwords.fetch_regexes.intercepted", include_bytes!("../../server/retail/machonet/bannedwords.fetch_regexes.intercepted.cache")),
    ("bannedwords.fetch_regexes.blocked", include_bytes!("../../server/retail/machonet/bannedwords.fetch_regexes.blocked.cache")),
];

fn calls() -> &'static HashMap<&'static str, Value> {
    static DECODED: OnceLock<HashMap<&'static str, Value>> = OnceLock::new();
    DECODED.get_or_init(|| {
        let mut out = HashMap::new();
        for (name, bytes) in CALLS {
            let lret = match marshal::loads(bytes) {
                Ok(Value::Tuple(t)) => t.get(1).and_then(|d| d.get("lret")).cloned(),
                Ok(_) => None,
                Err(e) => {
                    warn!(target: "infinity_server::retail", "retail: cannot decode {name}: {e:?}");
                    None
                }
            };
            match lret {
                Some(v) => {
                    out.insert(*name, v);
                }
                None => warn!(target: "infinity_server::retail", "retail: {name} has no result"),
            }
        }
        out
    })
}

/// The retail server's answer to `service.method` (with `.arg` for calls keyed by
/// their first argument), if it was captured.
pub fn cached_call(service: &str, method: &str, args: &[Value]) -> Option<Value> {
    let calls = calls();
    let key = format!("{service}.{method}");
    if let Some(v) = calls.get(key.as_str()) {
        return Some(v.clone());
    }
    let arg = args.first()?.as_text()?;
    calls.get(format!("{key}.{arg}").as_str()).cloned()
}

/// Retail average market prices: typeID -> (averagePrice, adjustedPrice) in ISK.
pub fn average_prices() -> Vec<(i32, f64, f64)> {
    let Some(Value::Dict(prices)) = calls().get("config.GetAverageMarketPrices") else { return vec![] };
    prices
        .iter()
        .filter_map(|(k, v)| {
            let state = match v {
                Value::Instance(_, state) => state.as_ref(),
                other => other,
            };
            let num = |name: &str| state.get(name).and_then(|x| x.as_float()).unwrap_or(0.0);
            Some((k.as_int()? as i32, num("averagePrice"), num("adjustedPrice")))
        })
        .collect()
}

#[cfg(test)] mod tests;
