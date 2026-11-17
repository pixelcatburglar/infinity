//! Corporation vote persistence for `voting.py`: cases, options, holder snapshots, ballots
//! and claims for every vote type, plus blueprint reservations and the active CEO election
//! pointer on game.corporations.
//! Sanctioned-action rows live in sanctions.rs; policy, transactions and notifications are
//! not handled here.
use crate::db::{self, executor::Executor};

pub(crate) struct Case { pub(crate) id: i64, pub(crate) corporation_id: i64, pub(crate) creator_id: i64, pub(crate) title: String, pub(crate) description: String, pub(crate) vote_type: i64, pub(crate) starts: i64, pub(crate) ends: i64, pub(crate) closed_at: Option<i64>, pub(crate) reserved_item_id: Option<i64>, pub(crate) previous_ceo_id: Option<i64>, pub(crate) previous_ceo_was_active: Option<bool> }
impl Case { fn from_row(r: &tokio_postgres::Row) -> Self { Self { id: r.get("id"), corporation_id: r.get("corporation_id"), creator_id: r.get("creator_id"), title: r.get("title"), description: r.get("description"), vote_type: r.get("vote_type"), starts: r.get("starts"), ends: r.get("ends"), closed_at: r.get("closed_at"), reserved_item_id: r.get("reserved_item_id"), previous_ceo_id: r.get("previous_ceo_id"), previous_ceo_was_active: r.get("previous_ceo_was_active") } } }
pub(crate) struct NewCase<'a> { pub(crate) corporation_id: i64, pub(crate) creator_id: i64, pub(crate) title: &'a str, pub(crate) description: &'a str, pub(crate) vote_type: i64, pub(crate) starts: i64, pub(crate) ends: i64, pub(crate) previous_ceo_id: Option<i64>, pub(crate) previous_ceo_was_active: Option<bool> }
pub(crate) struct OptionRow { pub(crate) option_id: i32, pub(crate) text: String, pub(crate) parameter: Option<i64>, pub(crate) parameter1: Option<i64>, pub(crate) parameter2: Option<i64>, pub(crate) votes_for: i64 }
impl OptionRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { option_id: r.get("option_id"), text: r.get("text"), parameter: r.get("parameter"), parameter1: r.get("parameter1"), parameter2: r.get("parameter2"), votes_for: r.get("votes_for") } } }
pub(crate) struct NewOption<'a> { pub(crate) option_id: i32, pub(crate) text: &'a str, pub(crate) parameter: Option<i64>, pub(crate) parameter1: Option<i64>, pub(crate) parameter2: Option<i64> }
pub(crate) struct Ballot { pub(crate) character_id: i64, pub(crate) option_id: i32, pub(crate) weight: i64 }
impl Ballot { fn from_row(r: &tokio_postgres::Row) -> Self { Self { character_id: r.get("character_id"), option_id: r.get("option_id"), weight: r.get("weight") } } }
pub(crate) struct EligibleHolder { pub(crate) holder_id: i64 }
impl EligibleHolder { fn from_row(r: &tokio_postgres::Row) -> Self { Self { holder_id: r.get("holder_id") } } }
/// A scheduler deadline: the case to handle when due_at arrives.
pub(crate) struct Deadline { pub(crate) case_id: i64, pub(crate) due_at: i64 }
impl Deadline { fn from_row(r: &tokio_postgres::Row) -> Self { Self { case_id: r.get("case_id"), due_at: r.get("due_at") } } }
struct Number { value: i64 }
impl Number { fn from_row(r: &tokio_postgres::Row) -> Self { Self { value: r.get("value") } } }

const CASE_COLUMNS: &str = "id,corporation_id,creator_id,title,description,vote_type,starts,ends,closed_at,reserved_item_id,previous_ceo_id,previous_ceo_was_active";
const ALIASED_CASE_COLUMNS: &str = "c.id,c.corporation_id,c.creator_id,c.title,c.description,c.vote_type,c.starts,c.ends,c.closed_at,c.reserved_item_id,c.previous_ceo_id,c.previous_ceo_was_active";

pub(crate) async fn create(ex: &(impl Executor + ?Sized), case: &NewCase<'_>) -> Result<i64, db::Error> {
    Ok(Number::from_row(&ex.query_one("INSERT INTO game.corp_vote_cases (corporation_id,creator_id,title,description,vote_type,starts,ends,previous_ceo_id,previous_ceo_was_active) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id AS value", &[&case.corporation_id,&case.creator_id,&case.title,&case.description,&case.vote_type,&case.starts,&case.ends,&case.previous_ceo_id,&case.previous_ceo_was_active]).await?).value)
}
pub(crate) async fn add_option(ex: &(impl Executor + ?Sized), case: i64, option: &NewOption<'_>) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.corp_vote_options (case_id,option_id,text,parameter,parameter1,parameter2) VALUES ($1,$2,$3,$4,$5,$6)", &[&case,&option.option_id,&option.text,&option.parameter,&option.parameter1,&option.parameter2]).await?; Ok(())
}
pub(crate) async fn snapshot(ex: &(impl Executor + ?Sized), case: i64, issuer: i64) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.corp_vote_snapshot (case_id,holder_id,shares) SELECT $1,shareholder_id,shares FROM game.corp_shares WHERE corporation_id=$2 AND shareholder_id<>$2 AND shares>0 ORDER BY shareholder_id ON CONFLICT (case_id,holder_id) DO NOTHING", &[&case,&issuer]).await
}
pub(crate) async fn case(ex: &(impl Executor + ?Sized), case_id: i64) -> Result<Option<Case>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT {CASE_COLUMNS} FROM game.corp_vote_cases WHERE id=$1"));
    Ok(ex.query_opt(SQL.as_str(), &[&case_id]).await?.as_ref().map(Case::from_row))
}
pub(crate) async fn lock_case(ex: &(impl Executor + ?Sized), case_id: i64) -> Result<Option<Case>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT {CASE_COLUMNS} FROM game.corp_vote_cases WHERE id=$1 FOR UPDATE"));
    Ok(ex.query_opt(SQL.as_str(), &[&case_id]).await?.as_ref().map(Case::from_row))
}
pub(crate) async fn cases(ex: &(impl Executor + ?Sized), issuer: i64, now: i64, closed_limit: i64) -> Result<Vec<Case>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT * FROM (SELECT {CASE_COLUMNS} FROM game.corp_vote_cases WHERE corporation_id=$1 AND closed_at IS NULL AND ends>$2 UNION ALL (SELECT {CASE_COLUMNS} FROM game.corp_vote_cases WHERE corporation_id=$1 AND (closed_at IS NOT NULL OR ends<=$2) ORDER BY COALESCE(closed_at,ends) DESC,id DESC LIMIT $3)) c ORDER BY starts DESC,id DESC"));
    Ok(ex.query(SQL.as_str(), &[&issuer,&now,&closed_limit]).await?.iter().map(Case::from_row).collect())
}
pub(crate) async fn open_count(ex: &(impl Executor + ?Sized), issuer: i64, now: i64) -> Result<i64, db::Error> {
    Ok(Number::from_row(&ex.query_one("SELECT count(*) AS value FROM game.corp_vote_cases WHERE corporation_id=$1 AND closed_at IS NULL AND ends>$2", &[&issuer,&now]).await?).value)
}
pub(crate) async fn options(ex: &(impl Executor + ?Sized), case: i64) -> Result<Vec<OptionRow>, db::Error> {
    Ok(ex.query("SELECT o.option_id,o.text,o.parameter,o.parameter1,o.parameter2,COALESCE(sum(s.shares),0)::bigint AS votes_for FROM game.corp_vote_options o LEFT JOIN game.corp_vote_ballots b ON b.case_id=o.case_id AND b.option_id=o.option_id LEFT JOIN game.corp_vote_claims c ON c.case_id=b.case_id AND c.character_id=b.character_id LEFT JOIN game.corp_vote_snapshot s ON s.case_id=c.case_id AND s.holder_id=c.holder_id WHERE o.case_id=$1 GROUP BY o.option_id,o.text,o.parameter,o.parameter1,o.parameter2 ORDER BY o.option_id", &[&case]).await?.iter().map(OptionRow::from_row).collect())
}
pub(crate) async fn ballot(ex: &(impl Executor + ?Sized), case: i64, actor: i64) -> Result<Option<Ballot>, db::Error> {
    Ok(ex.query_opt("SELECT b.character_id,b.option_id,COALESCE(sum(s.shares),0)::bigint AS weight FROM game.corp_vote_ballots b LEFT JOIN game.corp_vote_claims c ON c.case_id=b.case_id AND c.character_id=b.character_id LEFT JOIN game.corp_vote_snapshot s ON s.case_id=c.case_id AND s.holder_id=c.holder_id WHERE b.case_id=$1 AND b.character_id=$2 GROUP BY b.character_id,b.option_id", &[&case,&actor]).await?.as_ref().map(Ballot::from_row))
}
pub(crate) async fn eligible_holders(ex: &(impl Executor + ?Sized), case: i64, actor: i64) -> Result<Vec<EligibleHolder>, db::Error> {
    Ok(ex.query("SELECT s.holder_id FROM game.corp_vote_snapshot s LEFT JOIN game.corp_vote_claims cl ON cl.case_id=s.case_id AND cl.holder_id=s.holder_id WHERE s.case_id=$1 AND cl.holder_id IS NULL AND (s.holder_id=$2 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.holder_id AND NOT c.deleted AND c.ceo_id=$2)) ORDER BY s.holder_id", &[&case,&actor]).await?.iter().map(EligibleHolder::from_row).collect())
}
/// Non-deleted corporations whose shares the actor may vote as their CEO. The service uses
/// the list to build its admission lock set before the case is locked.
pub(crate) async fn proxied_corporations(ex: &(impl Executor + ?Sized), actor: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT corporation_id AS value FROM game.corporations WHERE ceo_id=$1 AND NOT deleted ORDER BY corporation_id", &[&actor]).await?.iter().map(|r| Number::from_row(r).value).collect())
}
pub(crate) async fn insert_ballot(ex: &(impl Executor + ?Sized), case: i64, actor: i64, option: i32, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_vote_ballots (case_id,character_id,option_id,created) SELECT c.id,$2,o.option_id,$4 FROM game.corp_vote_cases c JOIN game.corp_vote_options o ON o.case_id=c.id AND o.option_id=$3 WHERE c.id=$1 AND c.closed_at IS NULL AND c.ends>$4 ON CONFLICT (case_id,character_id) DO NOTHING", &[&case,&actor,&option,&now]).await? != 0)
}
pub(crate) async fn claim_holder(ex: &(impl Executor + ?Sized), case: i64, holder: i64, actor: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_vote_claims (case_id,holder_id,character_id) SELECT s.case_id,s.holder_id,$3 FROM game.corp_vote_snapshot s WHERE s.case_id=$1 AND s.holder_id=$2 AND (s.holder_id=$3 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.holder_id AND NOT c.deleted AND c.ceo_id=$3)) ON CONFLICT (case_id,holder_id) DO NOTHING", &[&case,&holder,&actor]).await? != 0)
}
pub(crate) async fn close(ex: &(impl Executor + ?Sized), case: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corp_vote_cases SET closed_at=$2 WHERE id=$1 AND closed_at IS NULL AND ends<=$2", &[&case,&now]).await? != 0)
}
/// Reserves a blueprint item for an open case. A second reservation on the same case fails;
/// a conflict with another case's reservation surfaces as a unique-violation error.
pub(crate) async fn reserve_item(ex: &(impl Executor + ?Sized), case: i64, item: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corp_vote_cases SET reserved_item_id=$2 WHERE id=$1 AND closed_at IS NULL AND reserved_item_id IS NULL", &[&case,&item]).await? != 0)
}
/// Clears the reservation only while it still points at the given item.
pub(crate) async fn release_reservation(ex: &(impl Executor + ?Sized), case: i64, item: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corp_vote_cases SET reserved_item_id=NULL WHERE id=$1 AND reserved_item_id=$2", &[&case,&item]).await? != 0)
}
/// Points the corporation at its CEO election case, only while the recorded CEO still
/// matches and no other election is active.
pub(crate) async fn begin_election(ex: &(impl Executor + ?Sized), corp: i64, case: i64, previous_ceo: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corporations SET active_ceo_vote_id=$2 WHERE corporation_id=$1 AND ceo_id=$3 AND active_ceo_vote_id IS NULL", &[&corp,&case,&previous_ceo]).await? != 0)
}
/// Clears the election pointer only while it still references this case.
pub(crate) async fn end_election(ex: &(impl Executor + ?Sized), corp: i64, case: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corporations SET active_ceo_vote_id=NULL WHERE corporation_id=$1 AND active_ceo_vote_id=$2", &[&corp,&case]).await? != 0)
}
/// The case the corporation's active election pointer references, if any.
pub(crate) async fn election(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Option<Case>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT {ALIASED_CASE_COLUMNS} FROM game.corp_vote_cases c JOIN game.corporations o ON o.active_ceo_vote_id=c.id WHERE o.corporation_id=$1"));
    Ok(ex.query_opt(SQL.as_str(), &[&corp]).await?.as_ref().map(Case::from_row))
}
/// Every unclosed case and the end time its close job is due at, for scheduler seeding.
pub(crate) async fn open_deadlines(ex: &(impl Executor + ?Sized)) -> Result<Vec<Deadline>, db::Error> {
    Ok(ex.query("SELECT id AS case_id,ends AS due_at FROM game.corp_vote_cases WHERE closed_at IS NULL ORDER BY id", &[]).await?.iter().map(Deadline::from_row).collect())
}
pub(crate) async fn can_view(ex: &(impl Executor + ?Sized), issuer: i64, actor: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 WHERE EXISTS (SELECT 1 FROM game.corp_shares s WHERE s.corporation_id=$1 AND s.shareholder_id<>$1 AND s.shares>0 AND (s.shareholder_id=$2 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.shareholder_id AND NOT c.deleted AND c.ceo_id=$2))) OR EXISTS (SELECT 1 FROM game.corp_vote_cases v JOIN game.corp_vote_snapshot s ON s.case_id=v.id WHERE v.corporation_id=$1 AND (s.holder_id=$2 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.holder_id AND NOT c.deleted AND c.ceo_id=$2))) OR EXISTS (SELECT 1 FROM game.corp_vote_cases v JOIN game.corp_vote_ballots b ON b.case_id=v.id WHERE v.corporation_id=$1 AND b.character_id=$2)", &[&issuer,&actor]).await?.is_some())
}
pub(crate) async fn observers(ex: &(impl Executor + ?Sized), case: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT character_id AS value FROM game.characters WHERE NOT deleted AND corporation_id=(SELECT corporation_id FROM game.corp_vote_cases WHERE id=$1) UNION SELECT ch.character_id AS value FROM game.corp_vote_snapshot s JOIN game.characters ch ON ch.character_id=s.holder_id AND NOT ch.deleted WHERE s.case_id=$1 UNION SELECT c.ceo_id AS value FROM game.corp_vote_snapshot s JOIN game.corporations c ON c.corporation_id=s.holder_id AND NOT c.deleted JOIN game.characters ch ON ch.character_id=c.ceo_id AND NOT ch.deleted WHERE s.case_id=$1 UNION SELECT character_id AS value FROM game.corp_vote_ballots WHERE case_id=$1 ORDER BY value", &[&case]).await?.iter().map(|r| Number::from_row(r).value).collect())
}
