//! UTC period boundaries and frozen assignment generations in game.daily_goal_periods.
//! Daily client semantics are supplied by the service; no clock or transaction is owned here.
use infinity_core::{consts::{FILETIME_SECOND, FILETIME_UNIX_EPOCH}};
use crate::{client, db::{self, executor::Executor}};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bounds { pub day_start: i64, pub day_end: i64, pub month_start: i64, pub month_end: i64 }

pub(crate) async fn bounds(ex: &(impl Executor + ?Sized), at: i64) -> Result<Bounds, db::Error> {
    // The shared seconds helper truncates toward zero; floor pre-epoch fractional seconds.
    let mut seconds = client::unix_seconds_from_filetime(at);
    if at < FILETIME_UNIX_EPOCH && (i128::from(FILETIME_UNIX_EPOCH) - i128::from(at)) % i128::from(FILETIME_SECOND) != 0 { seconds -= 1; }
    let r = ex.query_one("SELECT extract(epoch FROM date_trunc('day', to_timestamp($1::bigint) AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')::bigint AS ds, extract(epoch FROM (date_trunc('day', to_timestamp($1::bigint) AT TIME ZONE 'UTC') + interval '1 day') AT TIME ZONE 'UTC')::bigint AS de, extract(epoch FROM date_trunc('month', to_timestamp($1::bigint) AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')::bigint AS ms, extract(epoch FROM (date_trunc('month', to_timestamp($1::bigint) AT TIME ZONE 'UTC') + interval '1 month') AT TIME ZONE 'UTC')::bigint AS me", &[&seconds]).await?;
    let convert = |name: &str| -> Result<i64, db::Error> {
        let s: i64 = r.try_get(name)?;
        let ticks = i128::from(s) * i128::from(FILETIME_SECOND) + i128::from(FILETIME_UNIX_EPOCH);
        i64::try_from(ticks).map_err(|_| db::Error::Progress(anyhow::anyhow!("UTC boundary outside FILETIME range")))
    };
    Ok(Bounds { day_start: convert("ds")?, day_end: convert("de")?, month_start: convert("ms")?, month_end: convert("me")? })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeriodRow { pub character_id: i64, pub kind: String, pub period_start: i64, pub period_end: i64, pub assigned_at: i64, pub generation: i64, pub namespace: String, pub version: String, pub revision: i64 }
impl PeriodRow {
    fn from_row(r: &tokio_postgres::Row) -> Result<Self, db::Error> { Ok(Self { character_id: r.try_get("character_id")?, kind: r.try_get("kind")?, period_start: r.try_get("period_start")?, period_end: r.try_get("period_end")?, assigned_at: r.try_get("assigned_at")?, generation: r.try_get("generation")?, namespace: r.try_get("namespace")?, version: r.try_get("version")?, revision: r.try_get("revision")? }) }
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), character: i64, kind: &str, start: i64) -> Result<Option<PeriodRow>, db::Error> {
    ex.query_opt("SELECT character_id,kind,period_start,period_end,assigned_at,generation,namespace,version,revision FROM game.daily_goal_periods WHERE character_id=$1 AND kind=$2 AND period_start=$3", &[&character,&kind,&start]).await?.as_ref().map(PeriodRow::from_row).transpose()
}
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), p: &PeriodRow) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.daily_goal_periods(character_id,kind,period_start,period_end,assigned_at,generation,namespace,version,revision) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (character_id,kind,period_start) DO NOTHING", &[&p.character_id,&p.kind,&p.period_start,&p.period_end,&p.assigned_at,&p.generation,&p.namespace,&p.version,&p.revision]).await? == 1)
}
