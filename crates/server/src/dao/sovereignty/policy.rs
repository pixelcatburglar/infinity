//! Alliance capital policy persistence; appointment and cooldown decisions are caller-owned.
use crate::db::{self,executor::Executor};
#[derive(Clone,Debug,PartialEq)]
pub(crate) struct PolicyRow {
    pub alliance_id:i64, pub current_capital_system:Option<i64>, pub last_capital_changed_at:Option<i64>,
    pub capital_lost_at:Option<i64>, pub capital_change_not_before:Option<i64>, pub revision:i64,
}
impl PolicyRow { fn from_row(r:&tokio_postgres::Row)->Self { Self { alliance_id:r.get("alliance_id"), current_capital_system:r.get("current_capital_system"), last_capital_changed_at:r.get("last_capital_changed_at"), capital_lost_at:r.get("capital_lost_at"), capital_change_not_before:r.get("capital_change_not_before"), revision:r.get("revision") } } }
const COLS:&str="alliance_id,current_capital_system,last_capital_changed_at,capital_lost_at,capital_change_not_before,revision";
pub(crate) async fn read(ex:&(impl Executor+?Sized), alliance:i64)->Result<Option<PolicyRow>,db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLS} FROM game.sov_alliance_policy WHERE alliance_id=$1"),&[&alliance]).await?.map(|r|PolicyRow::from_row(&r)))
}
/// Lock policy before any system row, in the caller's transaction.
pub(crate) async fn lock(ex:&(impl Executor+?Sized), alliance:i64)->Result<PolicyRow,db::Error> {
    ex.execute("INSERT INTO game.sov_alliance_policy (alliance_id) VALUES ($1) ON CONFLICT DO NOTHING",&[&alliance]).await?;
    Ok(PolicyRow::from_row(&ex.query_one(&format!("SELECT {COLS} FROM game.sov_alliance_policy WHERE alliance_id=$1 FOR UPDATE"),&[&alliance]).await?))
}
pub(crate) async fn replace(ex:&(impl Executor+?Sized), expected_revision:i64, r:&PolicyRow)->Result<bool,db::Error> {
    Ok(ex.execute("UPDATE game.sov_alliance_policy SET current_capital_system=$3,last_capital_changed_at=$4,capital_lost_at=$5,capital_change_not_before=$6,revision=revision+1 WHERE alliance_id=$1 AND revision=$2",&[&r.alliance_id,&expected_revision,&r.current_capital_system,&r.last_capital_changed_at,&r.capital_lost_at,&r.capital_change_not_before]).await?==1)
}
