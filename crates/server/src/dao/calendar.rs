//! Calendar event and response persistence for `eve/client/script/ui/services/eveCalendarsvc.py`.
//! Uses `game.calendar_events` and `game.calendar_responses`; alliance events are not modelled.
//! Client responses, policy, transactions and notifications remain with callers.

use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct EventListRow {
    pub event_id: i64,
    pub owner_id: i64,
    pub event_date: i64,
    pub modified: i64,
    pub duration: i64,
    pub importance: i64,
    pub title: String,
    pub deleted: bool,
}
impl EventListRow {
    fn from_row(r: &Row) -> Self {
        Self {
            event_id: r.get("event_id"),
            owner_id: r.get("owner_id"),
            event_date: r.get("event_date"),
            modified: r.get("modified"),
            duration: r.get("duration"),
            importance: r.get("importance"),
            title: r.get("title"),
            deleted: r.get("deleted"),
        }
    }
}
pub(crate) struct Details {
    pub description: String,
    pub creator_id: i64,
    pub owner_id: i64,
}
impl Details {
    fn from_row(r: &Row) -> Self {
        Self { description: r.get("description"), creator_id: r.get("creator_id"), owner_id: r.get("owner_id") }
    }
}
pub(crate) struct OwnerDate {
    pub owner_id: i64,
    pub event_date: i64,
}
impl OwnerDate {
    fn from_row(r: &Row) -> Self {
        Self { owner_id: r.get("owner_id"), event_date: r.get("event_date") }
    }
}
pub(crate) struct EventResponse {
    pub event_id: i64,
    pub status: i64,
}
impl EventResponse {
    fn from_row(r: &Row) -> Self { Self { event_id: r.get("event_id"), status: r.get("status") } }
}
pub(crate) struct CharacterResponse {
    pub char_id: i64,
    pub status: i64,
}
impl CharacterResponse {
    fn from_row(r: &Row) -> Self { Self { char_id: r.get("char_id"), status: r.get("status") } }
}

pub(crate) async fn event_list(
    ex: &impl Executor, me: i64, corp: i64, month: i32, year: i32,
    deleted: i32, epoch: i64, second: i64,
) -> Result<Vec<EventListRow>, Error> {
    let rows = ex.query(
        "SELECT e.event_id, e.owner_id, e.event_date, e.modified, e.duration::bigint, e.importance::bigint, e.title, e.deleted
             FROM game.calendar_events e
             WHERE (e.owner_id IN ($1, $2) OR EXISTS (SELECT 1 FROM game.calendar_responses r WHERE r.event_id = e.event_id AND r.char_id = $1 AND r.status <> $5))
               AND extract(month FROM to_timestamp((e.event_date - $6::bigint) / ($7::bigint)::numeric))::int = $3
               AND extract(year FROM to_timestamp((e.event_date - $6::bigint) / ($7::bigint)::numeric))::int = $4
             ORDER BY e.event_date",
        &[&me, &corp, &month, &year, &deleted, &epoch, &second],
    ).await?;
    Ok(rows.iter().map(EventListRow::from_row).collect())
}
pub(crate) async fn invitees(ex: &impl Executor, event: i64, deleted: i32) -> Result<Vec<i64>, Error> {
    let rows = ex.query(
        "SELECT char_id FROM game.calendar_responses WHERE event_id = $1 AND status <> $2",
        &[&event, &deleted],
    ).await?;
    Ok(rows.iter().map(|r| r.get::<_, i64>("char_id")).collect())
}
pub(crate) struct NewEvent<'a> {
    pub owner: i64,
    pub creator: i64,
    pub when: i64,
    pub duration: i32,
    pub title: &'a str,
    pub description: &'a str,
    pub importance: i32,
    pub modified: i64,
}
pub(crate) async fn create(ex: &impl Executor, e: &NewEvent<'_>) -> Result<i64, Error> {
    let row = ex.query_one(
        "INSERT INTO game.calendar_events (owner_id, creator_id, event_date, duration, title, description, importance, modified)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING event_id",
        &[&e.owner, &e.creator, &e.when, &e.duration, &e.title, &e.description, &e.importance, &e.modified],
    ).await?;
    Ok(row.get::<_, i64>("event_id"))
}
pub(crate) async fn initial_responses(
    ex: &impl Executor, event: i64, me: i64, guests: &[i64], undecided: i32, accepted: i32,
) -> Result<(), Error> {
    let mut responses: Vec<_> = guests.iter().map(|&g| (g, undecided)).collect();
    responses.push((me, accepted));
    responses.sort_unstable_by_key(|&(char_id, _)| char_id);
    for (char_id, status) in responses {
        if char_id == me {
            ex.execute(
                "INSERT INTO game.calendar_responses (event_id, char_id, status) VALUES ($1, $2, $3)",
                &[&event, &char_id, &status],
            ).await?;
        } else {
            insert_response(ex, event, char_id, status).await?;
        }
    }
    Ok(())
}
pub(crate) async fn owner(ex: &impl Executor, event: i64) -> Result<Option<i64>, Error> {
    let row = ex.query_opt("SELECT owner_id FROM game.calendar_events WHERE event_id = $1", &[&event]).await?;
    Ok(row.as_ref().map(|r| r.get::<_, i64>("owner_id")))
}
pub(crate) async fn details(ex: &impl Executor, event: i64) -> Result<Option<Details>, Error> {
    let row = ex.query_opt("SELECT description, creator_id, owner_id FROM game.calendar_events WHERE event_id = $1", &[&event]).await?;
    Ok(row.as_ref().map(Details::from_row))
}
pub(crate) async fn owner_date(ex: &impl Executor, event: i64) -> Result<Option<OwnerDate>, Error> {
    let row = ex.query_opt("SELECT owner_id, event_date FROM game.calendar_events WHERE event_id = $1", &[&event]).await?;
    Ok(row.as_ref().map(OwnerDate::from_row))
}
pub(crate) struct EditEvent<'a> {
    pub event: i64,
    pub when: i64,
    pub duration: i32,
    pub title: &'a str,
    pub description: &'a str,
    pub importance: i32,
    pub modified: i64,
}
pub(crate) async fn edit(ex: &impl Executor, e: &EditEvent<'_>) -> Result<u64, Error> {
    ex.execute(
        "UPDATE game.calendar_events SET event_date = $2, duration = $3, title = $4, description = $5, importance = $6, modified = $7 WHERE event_id = $1",
        &[&e.event, &e.when, &e.duration, &e.title, &e.description, &e.importance, &e.modified],
    ).await
}
pub(crate) async fn delete_event(ex: &impl Executor, event: i64, modified: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.calendar_events SET deleted = true, modified = $2 WHERE event_id = $1", &[&event, &modified]).await
}
pub(crate) async fn delete_response(ex: &impl Executor, event: i64, me: i64, status: i32) -> Result<u64, Error> {
    ex.execute("UPDATE game.calendar_responses SET status = $3 WHERE event_id = $1 AND char_id = $2", &[&event, &me, &status]).await
}
pub(crate) async fn set_response(ex: &impl Executor, event: i64, me: i64, status: i32) -> Result<u64, Error> {
    ex.execute(
        "INSERT INTO game.calendar_responses (event_id, char_id, status) VALUES ($1, $2, $3) ON CONFLICT (event_id, char_id) DO UPDATE SET status = $3",
        &[&event, &me, &status],
    ).await
}
pub(crate) async fn character_responses(ex: &impl Executor, me: i64) -> Result<Vec<EventResponse>, Error> {
    let rows = ex.query("SELECT event_id, status::bigint FROM game.calendar_responses WHERE char_id = $1", &[&me]).await?;
    Ok(rows.iter().map(EventResponse::from_row).collect())
}
pub(crate) async fn event_responses(ex: &impl Executor, event: i64) -> Result<Vec<CharacterResponse>, Error> {
    let rows = ex.query("SELECT char_id, status::bigint FROM game.calendar_responses WHERE event_id = $1", &[&event]).await?;
    Ok(rows.iter().map(CharacterResponse::from_row).collect())
}
pub(crate) async fn add_response(ex: &impl Executor, event: i64, char_id: i64, status: i32) -> Result<u64, Error> {
    insert_response(ex, event, char_id, status).await
}
async fn insert_response(ex: &impl Executor, event: i64, char_id: i64, status: i32) -> Result<u64, Error> {
    ex.execute(
        "INSERT INTO game.calendar_responses (event_id, char_id, status) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        &[&event, &char_id, &status],
    ).await
}
pub(crate) async fn remove_responses(ex: &impl Executor, event: i64, gone: &[i64], me: i64) -> Result<u64, Error> {
    ex.execute(
        "DELETE FROM game.calendar_responses WHERE event_id = $1 AND char_id = ANY($2) AND char_id <> $3",
        &[&event, &gone, &me],
    ).await
}
