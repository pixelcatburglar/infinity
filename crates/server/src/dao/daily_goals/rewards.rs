//! Frozen daily reward assets in game.daily_goal_rewards.
//! Economy settlement and transaction boundaries belong to the caller.
use crate::{db::{self, executor::Executor}, isk::IskAmount};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RewardRow { pub goal_id: String, pub reward_index: i32, pub asset_id: String, pub unit: String, pub amount_isk: Option<IskAmount>, pub amount_units: Option<u64>, pub issuer_id: Option<i64>, pub type_id: Option<i32>, pub omega_required: bool, pub earned: bool, pub claimed_at: Option<i64>, pub location_id: Option<i64>, pub item_id: Option<i64>, pub plex_reference: Option<i64>, pub revision: i64 }
const COLUMNS: &str = "goal_id::text AS goal_id,reward_index,asset_id::text AS asset_id,unit,amount_isk::text AS amount_isk,amount_units::text AS amount_units,issuer_id,type_id,omega_required,earned,claimed_at,location_id,item_id,plex_reference,revision";
impl RewardRow {
    fn validate(&self) -> Result<(), db::Error> {
        let units = self.amount_units.unwrap_or(0);
        let valid = match self.unit.as_str() {
            "isk" => self.amount_isk.is_some_and(|v| v > IskAmount::ZERO && v.split_precision().is_some()) && self.amount_units.is_none() && self.issuer_id.is_none() && self.type_id.is_none(),
            "lp" => self.amount_isk.is_none() && units > 0 && units <= i64::MAX as u64 && self.issuer_id.is_some() && self.type_id.is_none(),
            "sp" => self.amount_isk.is_none() && units > 0 && units <= u32::MAX as u64 && self.issuer_id.is_none() && self.type_id.is_none(),
            "item" => self.amount_isk.is_none() && units > 0 && units <= i32::MAX as u64 && self.issuer_id.is_none() && self.type_id.is_some(),
            "plex" => self.amount_isk.is_none() && units > 0 && units.checked_mul(infinity_core::consts::PLEX_HUNDREDTHS as u64).is_some_and(|v| v <= i64::MAX as u64) && self.issuer_id.is_none() && self.type_id.is_none(),
            _ => false,
        };
        if !valid { return Err(db::Error::Progress(anyhow::anyhow!("invalid reward amount or shape"))); }
        Ok(())
    }
    fn from_row(r: &tokio_postgres::Row) -> Result<Self, db::Error> {
        let isk: Option<&str> = r.try_get("amount_isk")?; let units: Option<&str> = r.try_get("amount_units")?;
        let row = Self { goal_id:r.try_get("goal_id")?, reward_index:r.try_get("reward_index")?, asset_id:r.try_get("asset_id")?, unit:r.try_get("unit")?, amount_isk:isk.map(IskAmount::parse_decimal).transpose().map_err(|e|db::Error::Progress(e.into()))?, amount_units:units.map(str::parse).transpose().map_err(|e: std::num::ParseIntError|db::Error::Progress(e.into()))?, issuer_id:r.try_get("issuer_id")?, type_id:r.try_get("type_id")?, omega_required:r.try_get("omega_required")?, earned:r.try_get("earned")?, claimed_at:r.try_get("claimed_at")?, location_id:r.try_get("location_id")?, item_id:r.try_get("item_id")?, plex_reference:r.try_get("plex_reference")?, revision:r.try_get("revision")? };
        row.validate()?;
        Ok(row)
    }
}
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), owner: i64, r: &RewardRow) -> Result<bool, db::Error> {
    r.validate()?;
    if r.plex_reference.is_some() { return Err(db::Error::Progress(anyhow::anyhow!("new reward cannot have a PLEX reference"))); }
    let isk = r.amount_isk.map(IskAmount::decimal); let units = r.amount_units.map(|v|v.to_string());
    Ok(ex.execute("INSERT INTO game.daily_goal_rewards(goal_id,reward_index,asset_id,unit,amount_isk,amount_units,issuer_id,type_id,omega_required,earned,claimed_at,location_id,item_id,revision) SELECT goal_id,$2,$3::text::uuid,$4,$5::text::numeric,$6::text::numeric,$7,$8,$9,$10,$11,$12,$13,$14 FROM game.daily_goal_instances WHERE goal_id=$1::text::uuid AND character_id=$15 ON CONFLICT DO NOTHING", &[&r.goal_id,&r.reward_index,&r.asset_id,&r.unit,&isk,&units,&r.issuer_id,&r.type_id,&r.omega_required,&r.earned,&r.claimed_at,&r.location_id,&r.item_id,&r.revision,&owner]).await? == 1)
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized), owner: i64, goal: &str, lock: bool) -> Result<Vec<RewardRow>, db::Error> {
    let suffix = if lock { " FOR UPDATE OF rewards" } else { "" };
    ex.query(&format!("SELECT {COLUMNS} FROM game.daily_goal_rewards rewards WHERE goal_id=$1::text::uuid AND EXISTS (SELECT 1 FROM game.daily_goal_instances g WHERE g.goal_id=rewards.goal_id AND g.character_id=$2) ORDER BY reward_index{suffix}"), &[&goal,&owner]).await?.iter().map(RewardRow::from_row).collect()
}
/// Reads all reward rows for the requested owner's goals in one statement.
pub(crate) async fn for_goals(ex: &(impl Executor + ?Sized), owner: i64, ids: &[String]) -> Result<Vec<RewardRow>, db::Error> {
    ex.query(&format!("SELECT {COLUMNS} FROM game.daily_goal_rewards rewards WHERE goal_id=ANY(SELECT unnest($2::text[])::uuid) AND EXISTS (SELECT 1 FROM game.daily_goal_instances g WHERE g.goal_id=rewards.goal_id AND g.character_id=$1) ORDER BY goal_id,reward_index"), &[&owner,&ids]).await?.iter().map(RewardRow::from_row).collect()
}
pub(crate) async fn earn(ex: &(impl Executor + ?Sized), owner: i64, goal: &str, index: i32, revision: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.daily_goal_rewards rewards SET earned=true,revision=revision+1 WHERE goal_id=$1::text::uuid AND reward_index=$2 AND revision=$3 AND NOT earned AND claimed_at IS NULL AND EXISTS (SELECT 1 FROM game.daily_goal_instances g WHERE g.goal_id=rewards.goal_id AND g.character_id=$4)", &[&goal,&index,&revision,&owner]).await? == 1)
}
pub(crate) async fn claim(ex: &(impl Executor + ?Sized), owner: i64, goal: &str, index: i32, revision: i64, at: i64, location: Option<i64>, item: Option<i64>, plex_reference: Option<i64>) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.daily_goal_rewards rewards SET claimed_at=$4,location_id=$5,item_id=$6,plex_reference=$8,revision=revision+1 WHERE goal_id=$1::text::uuid AND reward_index=$2 AND revision=$3 AND earned AND claimed_at IS NULL AND ((unit='plex' AND $8::bigint > 0) OR (unit<>'plex' AND $8::bigint IS NULL)) AND EXISTS (SELECT 1 FROM game.daily_goal_instances g WHERE g.goal_id=rewards.goal_id AND g.character_id=$7)", &[&goal,&index,&revision,&at,&location,&item,&owner,&plex_reference]).await? == 1)
}
