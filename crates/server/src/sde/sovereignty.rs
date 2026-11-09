//! Validated Hub components from the server's `sde_native.spacecomponentsbytype` export.
//! Client readers include `spacecomponents/client/components/deploy.py`,
//! `entosis/spacecomponents/client/entosisCaptureTarget.py` and
//! `spacecomponents/common/helper.py` (absence of activate means active).
//! Deployment and capture state, command nodes and campaigns are not implemented here.
use anyhow::{bail, ensure, Context};
use serde_json::Value;
use infinity_core::{consts::{GROUP_SOVEREIGNTY_HUB, TYPE_SOVEREIGNTY_HUB}};
use crate::{dao::sde::sovereignty as dao, db::Db};
use super::types::Types;

#[cfg_attr(test, derive(Default))]
pub(crate) struct Sovereignty { hub: HubDefinition }

#[cfg_attr(test, derive(Default))]
pub(crate) struct HubDefinition {
    pub(crate) type_id: i32,
    /// `spacecomponentsbytype.physics.isAlwaysGlobal`, consumed by client physics.
    pub(crate) is_always_global: bool,
    pub(crate) deploy: DeployDefinition,
    pub(crate) capture: CaptureDefinition,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct DeployDefinition {
    pub(crate) default_owner: i64,
    pub(crate) requires_fleet: bool,
    pub(crate) launch_for_own_corp: bool,
    pub(crate) max_security_level: f64,
    pub(crate) requires_alliance: bool,
    pub(crate) fixed_star_distance_m: i64,
    pub(crate) max_per_solar_system: i64,
    pub(crate) disallow_if_cyno_blocked: bool,
    pub(crate) min_distance_from_own_group_m: f64,
    pub(crate) flag_as_suspect_when_deployed: bool,
    pub(crate) min_distance_from_control_tower_m: f64,
    pub(crate) disallow_in_warp_disrupted_dungeon: bool,
    pub(crate) min_distance_from_stargates_and_stations_m: f64,
    pub(crate) min_distance_from_ess_m: Option<f64>,
    pub(crate) min_distance_from_moon_beacon_m: Option<f64>,
    pub(crate) min_distance_from_wormhole_m: Option<f64>,
    pub(crate) max_distance_from_control_tower_m: Option<f64>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct CaptureDefinition {
    pub(crate) capture_time_seconds: f64,
    pub(crate) defensive_regen_rate: f64,
}

impl Sovereignty {
    pub(crate) async fn load(db: &Db, types: &Types) -> anyhow::Result<Self> {
        Self::from_rows(dao::load(db, TYPE_SOVEREIGNTY_HUB).await?, types.get(TYPE_SOVEREIGNTY_HUB).map(|t| t.group_id))
    }
    pub(crate) fn hub(&self) -> &HubDefinition { &self.hub }

    fn from_rows(rows: Vec<dao::ComponentRow>, group: Option<Option<i32>>) -> anyhow::Result<Self> {
        ensure!(group.is_some(), "Hub type reference missing: {TYPE_SOVEREIGNTY_HUB}");
        ensure!(group == Some(Some(GROUP_SOVEREIGNTY_HUB)), "Hub group reference mismatch: {group:?}");
        ensure!(!rows.is_empty(), "spacecomponentsbytype Hub row missing");
        ensure!(rows.len() == 1, "spacecomponentsbytype Hub duplicate or conflicting rows");
        let row = rows.into_iter().next().context("spacecomponentsbytype Hub row missing")?;
        ensure!(row.key.as_deref() == Some(TYPE_SOVEREIGNTY_HUB.to_string().as_str()), "spacecomponentsbytype.key mismatch");
        let data: Value = serde_json::from_str(&row.data).context("spacecomponentsbytype.data invalid JSON")?;
        ensure!(integer(&data, "_key")? == i64::from(TYPE_SOVEREIGNTY_HUB), "spacecomponentsbytype._key mismatch");
        ensure!(data.get("activate").is_none(), "spacecomponentsbytype.activate unsupported for Hub");
        let d = object(&data, "deploy")?;
        let c = object(&data, "entosisSovereigntyHub")?;
        let security = number(d, "maxSecurityLevel")?;
        ensure!((-1.0..=1.0).contains(&security), "deploy.maxSecurityLevel out of range");
        let owner = positive_integer(d, "defaultOwner")?;
        let star = nonnegative_integer(d, "fixedStarDistance")?;
        let max = positive_integer(d, "maxPerSolarSystem")?;
        let duration = number(c, "captureTimeSeconds")?;
        ensure!(duration > 0.0, "entosisSovereigntyHub.captureTimeSeconds must be positive");
        distance(d, "deployAtRange")?;
        for key in ["disallowInVoidSpace", "disallowInAbyssSpace", "disallowInWormholeSpace", "isForPlayerOwnableSpace", "disallowInTriglavianSpace"] {
            flag(d, key)?;
        }
        nonnegative_integer(c, "campaignEventType")?;
        let hub = HubDefinition {
            type_id: TYPE_SOVEREIGNTY_HUB,
            is_always_global: flag(object(&data, "physics")?, "isAlwaysGlobal")?,
            deploy: DeployDefinition {
                default_owner: owner,
                requires_fleet: flag(d, "requiresFleet")?, launch_for_own_corp: flag(d, "launchForOwnCorp")?,
                max_security_level: security, requires_alliance: flag(d, "requiresAlliance")?, fixed_star_distance_m: star,
                max_per_solar_system: max, disallow_if_cyno_blocked: flag(d, "disallowIfCynoBlocked")?,
                min_distance_from_own_group_m: distance(d, "minDistanceFromOwnGroup")?,
                flag_as_suspect_when_deployed: flag(d, "flagAsSuspectWhenDeployed")?, min_distance_from_control_tower_m: distance(d, "minDistanceFromControlTower")?,
                disallow_in_warp_disrupted_dungeon: flag(d, "disallowInWarpDisruptedDungeon")?, min_distance_from_stargates_and_stations_m: distance(d, "minDistanceFromStargatesAndStations")?,
                min_distance_from_ess_m: optional_distance(d, "minDistanceFromESS")?, min_distance_from_moon_beacon_m: optional_distance(d, "minDistanceFromMoonBeacon")?,
                min_distance_from_wormhole_m: optional_distance(d, "minDistanceFromWormhole")?, max_distance_from_control_tower_m: optional_distance(d, "maxDistanceFromControlTower")?,
            },
            capture: CaptureDefinition { capture_time_seconds: duration, defensive_regen_rate: distance(c, "defensiveRegenRate")? },
        };
        Ok(Self { hub })
    }
}

fn object<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a Value> {
    let v = value.get(key).with_context(|| format!("missing {key}"))?;
    ensure!(v.is_object(), "{key} must be an object");
    Ok(v)
}
fn integer(value: &Value, key: &str) -> anyhow::Result<i64> {
    value.get(key).with_context(|| format!("missing {key}"))?.as_i64().with_context(|| format!("{key} must be an integer"))
}
fn nonnegative_integer(value: &Value, key: &str) -> anyhow::Result<i64> {
    let n = integer(value, key)?;
    ensure!(n >= 0, "{key} must be nonnegative");
    Ok(n)
}
fn positive_integer(value: &Value, key: &str) -> anyhow::Result<i64> {
    let n = integer(value, key)?;
    ensure!(n > 0, "{key} must be positive");
    Ok(n)
}
fn number(value: &Value, key: &str) -> anyhow::Result<f64> {
    let n = value.get(key).with_context(|| format!("missing {key}"))?.as_f64().with_context(|| format!("{key} must be a number"))?;
    ensure!(n.is_finite(), "{key} must be finite");
    Ok(n)
}
fn distance(value: &Value, key: &str) -> anyhow::Result<f64> {
    let n = number(value, key)?;
    ensure!(n >= 0.0, "{key} must be nonnegative");
    Ok(n)
}
fn optional_distance(value: &Value, key: &str) -> anyhow::Result<Option<f64>> {
    if value.get(key).is_some() { Ok(Some(distance(value, key)?)) } else { Ok(None) }
}
fn flag(value: &Value, key: &str) -> anyhow::Result<bool> {
    match integer(value, key)? { 0 => Ok(false), 1 => Ok(true), _ => bail!("{key} must be numeric 0 or 1") }
}

#[cfg(test)] mod tests;
