//! Immutable goal snapshots and guarded state transitions in game.daily_goal_instances.
//! Client projections and progress policy are outside this DAO.
use crate::{db::{self, executor::Executor}, lua::events::combat::{BinaryAmount, amount::validate_remainder}};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GoalRow {
    pub goal_id: String, pub character_id: i64, pub kind: String, pub period_start: i64,
    pub period_end: i64, pub slot_key: String, pub generation: i64, pub namespace: String,
    pub version: String, pub category: String, pub definition: String, pub target: u64,
    pub progress: u64, pub remainder: BinaryAmount, pub completed_at: Option<i64>,
    pub earned_omega: bool, pub paid: bool, pub voided_at: Option<i64>, pub has_unclaimed: bool, pub revision: i64,
}
const COLUMNS: &str = "goal_id::text AS goal_id,character_id,kind,period_start,period_end,slot_key,generation,namespace,version,category,definition::text AS definition,target::text AS target,progress::text AS progress,remainder_coefficient,remainder_exponent,completed_at,earned_omega,paid,voided_at,has_unclaimed,revision";
impl GoalRow {
    fn from_row(r: &tokio_postgres::Row) -> Result<Self, db::Error> {
        let remainder = BinaryAmount { coefficient: r.try_get("remainder_coefficient")?, exponent: r.try_get("remainder_exponent")? };
        validate_remainder(&remainder).map_err(db::Error::Progress)?;
        let parse = |name: &str| -> Result<u64, db::Error> { r.try_get::<_, &str>(name)?.parse().map_err(|e: std::num::ParseIntError| db::Error::Progress(e.into())) };
        let progress = parse("progress")?;
        let target = parse("target")?;
        if progress == target && remainder.coefficient != "0" { return Err(db::Error::Progress(anyhow::anyhow!("completed goal has remainder"))); }
        Ok(Self { goal_id:r.try_get("goal_id")?, character_id:r.try_get("character_id")?, kind:r.try_get("kind")?, period_start:r.try_get("period_start")?, period_end:r.try_get("period_end")?, slot_key:r.try_get("slot_key")?, generation:r.try_get("generation")?, namespace:r.try_get("namespace")?, version:r.try_get("version")?, category:r.try_get("category")?, definition:r.try_get("definition")?, target, progress, remainder, completed_at:r.try_get("completed_at")?, earned_omega:r.try_get("earned_omega")?, paid:r.try_get("paid")?, voided_at:r.try_get("voided_at")?, has_unclaimed:r.try_get("has_unclaimed")?, revision:r.try_get("revision")? })
    }
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), owner: i64, id: &str) -> Result<Option<GoalRow>, db::Error> {
    ex.query_opt(&format!("SELECT {COLUMNS} FROM game.daily_goal_instances WHERE character_id=$1 AND goal_id=$2::text::uuid"), &[&owner,&id]).await?.as_ref().map(GoalRow::from_row).transpose()
}
pub(crate) async fn period(ex: &(impl Executor + ?Sized), owner: i64, kind: &str, start: i64) -> Result<Vec<GoalRow>, db::Error> {
    ex.query(&format!("SELECT {COLUMNS} FROM game.daily_goal_instances WHERE character_id=$1 AND kind=$2 AND period_start=$3 ORDER BY goal_id"), &[&owner,&kind,&start]).await?.iter().map(GoalRow::from_row).collect()
}
/// Discover an owner's complete candidate period union with a caller-supplied sentinel.
/// The limit applies in SQL before immutable snapshots are materialized.
pub(crate) async fn bounded_period_union(ex: &(impl Executor + ?Sized), owner: i64, days: &[i64], months: &[i64], limit: i64) -> Result<Vec<GoalRow>, db::Error> {
    if limit <= 0 { return Err(db::Error::Progress(anyhow::anyhow!("invalid goal union limit"))); }
    ex.query(&format!("SELECT {COLUMNS} FROM game.daily_goal_instances WHERE character_id=$1 AND ((kind='day' AND period_start=ANY($2::bigint[])) OR (kind='month' AND period_start=ANY($3::bigint[]))) ORDER BY goal_id LIMIT $4"), &[&owner,&days,&months,&limit]).await?.iter().map(GoalRow::from_row).collect()
}
fn validate_ids(ids: &[String]) -> Result<(), db::Error> {
    let canonical = |id: &str| id.len() == 36 && id.bytes().enumerate().all(|(i, c)| {
        if [8, 13, 18, 23].contains(&i) { c == b'-' } else { c.is_ascii_digit() || (b'a'..=b'f').contains(&c) }
    });
    if !ids.iter().all(|id| canonical(id)) || ids.windows(2).any(|p| p[0] >= p[1]) {
        return Err(db::Error::Progress(anyhow::anyhow!("goal IDs must be canonical, sorted and unique")));
    }
    Ok(())
}
pub(crate) async fn lock_sorted(ex: &(impl Executor + ?Sized), owner: i64, ids: &[String]) -> Result<Vec<GoalRow>, db::Error> {
    validate_ids(ids)?;
    let rows: Vec<GoalRow> = ex.query(&format!("SELECT {COLUMNS} FROM game.daily_goal_instances WHERE character_id=$1 AND goal_id=ANY(SELECT unnest($2::text[])::uuid) ORDER BY goal_id FOR UPDATE"), &[&owner,&ids]).await?.iter().map(GoalRow::from_row).collect::<Result<_, _>>()?;
    if rows.len() != ids.len() || rows.iter().zip(ids).any(|(row, id)| row.goal_id != *id) {
        return Err(db::Error::Progress(anyhow::anyhow!("incomplete goal set")));
    }
    Ok(rows)
}
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), g: &GoalRow) -> Result<bool, db::Error> {
    validate_remainder(&g.remainder).map_err(db::Error::Progress)?;
    if g.progress == g.target && g.remainder.coefficient != "0" { return Err(db::Error::Progress(anyhow::anyhow!("completed goal has remainder"))); }
    Ok(ex.execute("INSERT INTO game.daily_goal_instances(goal_id,character_id,kind,period_start,period_end,slot_key,generation,namespace,version,category,definition,target,progress,remainder_coefficient,remainder_exponent,completed_at,earned_omega,paid,voided_at,has_unclaimed,revision) VALUES ($1::text::uuid,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::text::jsonb,$12::text::numeric,$13::text::numeric,$14,$15,$16,$17,$18,$19,$20,$21) ON CONFLICT DO NOTHING", &[&g.goal_id,&g.character_id,&g.kind,&g.period_start,&g.period_end,&g.slot_key,&g.generation,&g.namespace,&g.version,&g.category,&g.definition,&g.target.to_string(),&g.progress.to_string(),&g.remainder.coefficient,&g.remainder.exponent,&g.completed_at,&g.earned_omega,&g.paid,&g.voided_at,&g.has_unclaimed,&g.revision]).await? == 1)
}
#[derive(Clone, Debug)]
pub(crate) struct ProgressUpdate { pub progress: u64, pub remainder: BinaryAmount, pub completed_at: Option<i64>, pub earned_omega: bool, pub paid: bool, pub has_unclaimed: bool }
pub(crate) async fn update_progress(ex: &(impl Executor + ?Sized), owner: i64, id: &str, revision: i64, u: &ProgressUpdate) -> Result<bool, db::Error> {
    validate_remainder(&u.remainder).map_err(db::Error::Progress)?;
    if u.completed_at.is_some() && u.remainder.coefficient != "0" { return Err(db::Error::Progress(anyhow::anyhow!("completed goal has remainder"))); }
    Ok(ex.execute("UPDATE game.daily_goal_instances SET progress=$4::text::numeric,remainder_coefficient=$5,remainder_exponent=$6,completed_at=$7,earned_omega=$8,paid=$9,has_unclaimed=$10,revision=revision+1 WHERE character_id=$1 AND goal_id=$2::text::uuid AND revision=$3 AND voided_at IS NULL AND completed_at IS NULL AND $4::text::numeric BETWEEN progress AND target AND (($7::bigint IS NULL AND $4::text::numeric < target AND NOT $8 AND NOT $9 AND NOT $10) OR ($7::bigint IS NOT NULL AND $4::text::numeric = target))", &[&owner,&id,&revision,&u.progress.to_string(),&u.remainder.coefficient,&u.remainder.exponent,&u.completed_at,&u.earned_omega,&u.paid,&u.has_unclaimed]).await? == 1)
}
pub(crate) async fn set_unclaimed(ex: &(impl Executor + ?Sized), owner: i64, id: &str, revision: i64, value: bool) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.daily_goal_instances SET has_unclaimed=$4,revision=revision+1 WHERE character_id=$1 AND goal_id=$2::text::uuid AND revision=$3 AND completed_at IS NOT NULL", &[&owner,&id,&revision,&value]).await? == 1)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cursor { pub completed_at: i64, pub goal_id: String }
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryRow {
    pub goal_id: String,
    pub completed_at: i64,
    pub namespace: String,
    pub version: String,
}
impl HistoryRow {
    fn from_row(r: &tokio_postgres::Row) -> Result<Self, db::Error> {
        Ok(Self { goal_id: r.try_get("goal_id")?, completed_at: r.try_get("completed_at")?, namespace: r.try_get("namespace")?, version: r.try_get("version")? })
    }
}
pub(crate) async fn history(ex: &(impl Executor + ?Sized), owner: i64, high: Option<&Cursor>, last: Option<&Cursor>, limit: i64, maximum: i64) -> Result<Vec<HistoryRow>, db::Error> {
    if maximum <= 0 || limit <= 0 || limit > maximum { return Err(db::Error::Progress(anyhow::anyhow!("invalid history limit"))); }
    let high_time = high.map(|v|v.completed_at); let high_id = high.map(|v|v.goal_id.as_str());
    let last_time = last.map(|v|v.completed_at); let last_id = last.map(|v|v.goal_id.as_str());
    ex.query("SELECT goal_id::text AS goal_id,completed_at,namespace,version FROM game.daily_goal_instances WHERE character_id=$1 AND has_unclaimed AND completed_at IS NOT NULL AND ($2::bigint IS NULL OR (completed_at,goal_id)<=($2,$3::text::uuid)) AND ($4::bigint IS NULL OR (completed_at,goal_id)<($4,$5::text::uuid)) ORDER BY completed_at DESC,goal_id DESC LIMIT $6", &[&owner,&high_time,&high_id,&last_time,&last_id,&limit]).await?.iter().map(HistoryRow::from_row).collect()
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CompletedSource { pub goal_id: String, pub period_start: i64 }
pub(crate) async fn completed_sources(ex: &(impl Executor + ?Sized), owner: i64, start: i64, end: i64) -> Result<Vec<CompletedSource>, db::Error> {
    let rows = ex.query("SELECT goal_id::text AS goal_id,period_start FROM game.daily_goal_instances WHERE character_id=$1 AND kind='day' AND period_start >= $2 AND period_start < $3 AND category='bonus' AND completed_at IS NOT NULL ORDER BY goal_id,period_start", &[&owner,&start,&end]).await?;
    rows.iter().map(|r| Ok(CompletedSource { goal_id: r.try_get("goal_id")?, period_start: r.try_get("period_start")? })).collect()
}
