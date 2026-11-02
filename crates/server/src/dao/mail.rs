//! Persistence for mail and notifications, mirroring the data used by
//! `eve/client/script/ui/services/mail/mailSvc.py` and `notificationSvc.py`.
//! Uses `game.mail_messages`, `game.mail_status`, `game.mail_labels` and
//! `game.notifications`. List recipients are stored on messages; protocol values
//! and delivery effects remain in the services.

use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

/// Server notification policy: maximum rows returned.
const MAX_NOTIFICATIONS: i32 = 1000;

pub(crate) struct Header {
    pub message_id: i64,
    pub sender_id: i64,
    pub to_character_ids: String,
    pub to_list_id: Option<i64>,
    pub to_corp_or_alliance_id: Option<i64>,
    pub title: String,
    pub sent_date: i64,
}
impl Header {
    fn from_row(r: &Row) -> Self {
        Self {
            message_id: r.get("message_id"),
            sender_id: r.get("sender_id"),
            to_character_ids: r.get("to_character_ids"),
            to_list_id: r.get("to_list_id"),
            to_corp_or_alliance_id: r.get("to_corp_or_alliance_id"),
            title: r.get("title"),
            sent_date: r.get("sent_date"),
        }
    }
}
pub(crate) struct Status {
    pub message_id: i64,
    pub status_mask: i64,
    pub label_mask: i64,
}
impl Status {
    fn from_row(r: &Row) -> Self {
        Self { message_id: r.get("message_id"), status_mask: r.get("status_mask"), label_mask: r.get("label_mask") }
    }
}
pub(crate) struct Label {
    pub label_id: i64,
    pub name: String,
    pub color: Option<i64>,
}
impl Label {
    fn from_row(r: &Row) -> Self {
        Self { label_id: r.get("label_id"), name: r.get("name"), color: r.get("color") }
    }
}
struct LabelId { label_id: i32 }
impl LabelId {
    fn from_row(r: &Row) -> Self { Self { label_id: r.get("label_id") } }
}
struct MessageId { message_id: i64 }
impl MessageId {
    fn from_row(r: &Row) -> Self { Self { message_id: r.get("message_id") } }
}
struct Body { body: String }
impl Body {
    fn from_row(r: &Row) -> Self { Self { body: r.get("body") } }
}
pub(crate) struct Notification {
    pub notification_id: i64,
    pub type_id: i64,
    pub sender_id: i64,
    pub processed: bool,
    pub created: i64,
    pub data: String,
}
impl Notification {
    fn from_row(r: &Row) -> Self {
        Self {
            notification_id: r.get("notification_id"),
            type_id: r.get("type_id"),
            sender_id: r.get("sender_id"),
            processed: r.get("processed"),
            created: r.get("created"),
            data: r.get("data"),
        }
    }
}
struct NotificationId { notification_id: i64 }
impl NotificationId {
    fn from_row(r: &Row) -> Self { Self { notification_id: r.get("notification_id") } }
}

const HEADER_SELECT: &str = "SELECT m.message_id, m.sender_id, m.to_character_ids, m.to_list_id, m.to_corp_or_alliance_id, m.title, m.sent_date
    FROM game.mail_messages m JOIN game.mail_status s ON s.message_id = m.message_id";

pub(crate) async fn new_headers(ex: &impl Executor, me: i64, last_id: i64) -> Result<Vec<Header>, Error> {
    Ok(ex.query(&format!("{HEADER_SELECT} WHERE s.char_id = $1 AND m.message_id > $2 ORDER BY m.message_id"), &[&me, &last_id])
        .await?.iter().map(Header::from_row).collect())
}
pub(crate) async fn headers(ex: &impl Executor, me: i64, wanted: &Vec<i64>) -> Result<Vec<Header>, Error> {
    Ok(ex.query(&format!("{HEADER_SELECT} WHERE s.char_id = $1 AND m.message_id = ANY($2)"), &[&me, wanted])
        .await?.iter().map(Header::from_row).collect())
}
pub(crate) async fn statuses(ex: &impl Executor, me: i64) -> Result<Vec<Status>, Error> {
    Ok(ex.query("SELECT message_id, status_mask::bigint, label_mask::bigint FROM game.mail_status WHERE char_id = $1", &[&me])
        .await?.iter().map(Status::from_row).collect())
}
pub(crate) async fn insert_message(ex: &(impl Executor + ?Sized), me: i64, to_text: &str, to_list: Option<i64>, to_corp: Option<i64>, title: &str, body: &str, sent: i64) -> Result<i64, Error> {
    let row = ex.query_one(
        "INSERT INTO game.mail_messages (sender_id, to_character_ids, to_list_id, to_corp_or_alliance_id, title, body, sent_date)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING message_id",
        &[&me, &to_text, &to_list, &to_corp, &title, &body, &sent],
    ).await?;
    Ok(MessageId::from_row(&row).message_id)
}
pub(crate) async fn insert_statuses(ex: &(impl Executor + ?Sized), id: i64, rows: &[(i64, i32, i32)]) -> Result<u64, Error> {
    let (mut characters, mut statuses, mut labels) = (Vec::with_capacity(rows.len()), Vec::with_capacity(rows.len()), Vec::with_capacity(rows.len()));
    for &(character, status, label) in rows {
        characters.push(character);
        statuses.push(status);
        labels.push(label);
    }
    ex.execute(
        "INSERT INTO game.mail_status (message_id, char_id, status_mask, label_mask)
         SELECT $1, char_id, status_mask, label_mask FROM unnest($2::bigint[], $3::integer[], $4::integer[])
         AS rows(char_id, status_mask, label_mask) ORDER BY char_id ON CONFLICT DO NOTHING",
        &[&id, &characters, &statuses, &labels],
    ).await
}
pub(crate) async fn mark_original(ex: &impl Executor, original: i64, me: i64, bit: i32) -> Result<u64, Error> {
    ex.execute("UPDATE game.mail_status SET status_mask = status_mask | $3 WHERE message_id = $1 AND char_id = $2", &[&original, &me, &bit]).await
}
pub(crate) async fn body(ex: &impl Executor, id: i64, me: i64) -> Result<Option<String>, Error> {
    Ok(ex.query_opt("SELECT m.body FROM game.mail_messages m JOIN game.mail_status s USING (message_id) WHERE m.message_id = $1 AND s.char_id = $2", &[&id, &me])
        .await?.as_ref().map(Body::from_row).map(|r| r.body))
}

#[derive(Clone, Copy)]
pub(crate) enum Selector { Ids, Label, List, All }

pub(crate) async fn set_bits(ex: &impl Executor, me: i64, which: Selector, ids: &[i64], bits: i32, on: bool, trashed: i32) -> Result<u64, Error> {
    let (which, mask) = match which {
        Selector::Label => ("label_mask & ($2::bigint[])[1]::int <> 0 AND status_mask & $4::int = 0", Some(trashed)),
        Selector::List if bits == trashed => ("message_id IN (SELECT message_id FROM game.mail_messages WHERE to_list_id = ($2::bigint[])[1])", None),
        Selector::List => ("message_id IN (SELECT message_id FROM game.mail_messages WHERE to_list_id = ($2::bigint[])[1]) AND status_mask & $4::int = 0", Some(trashed)),
        Selector::All => ("$2::bigint[] IS NOT NULL", None),
        Selector::Ids => ("message_id = ANY($2)", None),
    };
    let set = if on { "status_mask | $3::int" } else { "status_mask & ~($3::int)" };
    let sql = format!("UPDATE game.mail_status SET status_mask = {set} WHERE char_id = $1 AND {which}");
    let ids = ids.to_vec();
    if let Some(mask) = mask {
        ex.execute(&sql, &[&me, &ids, &bits, &mask]).await
    } else {
        ex.execute(&sql, &[&me, &ids, &bits]).await
    }
}

pub(crate) async fn labels(ex: &impl Executor, me: i64) -> Result<Vec<Label>, Error> {
    Ok(ex.query("SELECT label_id::bigint, name, color::bigint FROM game.mail_labels WHERE char_id = $1", &[&me])
        .await?.iter().map(Label::from_row).collect())
}
pub(crate) async fn label_ids(ex: &impl Executor, me: i64) -> Result<Vec<i32>, Error> {
    Ok(ex.query("SELECT label_id FROM game.mail_labels WHERE char_id = $1", &[&me])
        .await?.iter().map(LabelId::from_row).map(|r| r.label_id).collect())
}
pub(crate) async fn insert_label(ex: &impl Executor, me: i64, id: i32, name: &str, color: Option<i32>) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.mail_labels (char_id, label_id, name, color) VALUES ($1, $2, $3, $4)", &[&me, &id, &name, &color]).await
}
pub(crate) async fn empty_trash(ex: &impl Executor, me: i64, trashed: i32) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.mail_status WHERE char_id = $1 AND status_mask & $2 <> 0", &[&me, &trashed]).await
}
pub(crate) async fn delete_mail(ex: &impl Executor, me: i64, wanted: &Vec<i64>) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.mail_status WHERE char_id = $1 AND message_id = ANY($2)", &[&me, wanted]).await
}
pub(crate) async fn edit_label(ex: &impl Executor, me: i64, id: i32, name: Option<String>, color: Option<i32>) -> Result<u64, Error> {
    ex.execute("UPDATE game.mail_labels SET name = COALESCE($3, name), color = COALESCE($4, color) WHERE char_id = $1 AND label_id = $2", &[&me, &id, &name, &color]).await
}
pub(crate) async fn delete_label(ex: &impl Executor, me: i64, id: i32) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.mail_labels WHERE char_id = $1 AND label_id = $2", &[&me, &id]).await
}
pub(crate) async fn clear_label(ex: &impl Executor, me: i64, id: i32) -> Result<u64, Error> {
    ex.execute("UPDATE game.mail_status SET label_mask = label_mask & ~($2::int) WHERE char_id = $1", &[&me, &id]).await
}

#[derive(Clone, Copy)]
pub(crate) enum LabelAction { Assign, Remove, Replace }

pub(crate) async fn change_labels(ex: &impl Executor, me: i64, wanted: &Vec<i64>, label: i32, action: LabelAction, builtin: i32) -> Result<u64, Error> {
    let (set, mask) = match action {
        LabelAction::Assign => ("label_mask | $3::int", None),
        LabelAction::Remove => ("label_mask & ~($3::int)", None),
        LabelAction::Replace => ("(label_mask & $4::int) | $3::int", Some(builtin)),
    };
    let sql = format!("UPDATE game.mail_status SET label_mask = {set} WHERE char_id = $1 AND message_id = ANY($2)");
    if let Some(mask) = mask {
        ex.execute(&sql, &[&me, wanted, &label, &mask]).await
    } else {
        ex.execute(&sql, &[&me, wanted, &label]).await
    }
}

pub(crate) async fn insert_notification(ex: &(impl Executor + ?Sized), receiver: i64, type_id: i32, group: i32, sender: i64, created: i64, data: &str) -> Result<i64, Error> {
    let row = ex.query_one(
        "INSERT INTO game.notifications (receiver_id, type_id, group_id, sender_id, created, data) VALUES ($1, $2, $3, $4, $5, $6) RETURNING notification_id",
        &[&receiver, &type_id, &group, &sender, &created, &data],
    ).await?;
    Ok(NotificationId::from_row(&row).notification_id)
}

#[derive(Clone, Copy)]
pub(crate) enum NotificationFilter { After, Unprocessed, Group }

pub(crate) async fn notifications(ex: &impl Executor, me: i64, filter: NotificationFilter, arg: i64) -> Result<Vec<Notification>, Error> {
    let where_sql = match filter {
        NotificationFilter::After => "notification_id > $2",
        NotificationFilter::Unprocessed => "NOT processed AND $2::bigint IS NOT NULL",
        NotificationFilter::Group => "group_id::bigint = $2",
    };
    Ok(ex.query(
        &format!(
            "SELECT notification_id, type_id::bigint, sender_id, processed, created, data FROM game.notifications
                 WHERE receiver_id = $1 AND {where_sql} ORDER BY notification_id DESC LIMIT {MAX_NOTIFICATIONS}"
        ),
        &[&me, &arg],
    ).await?.iter().map(Notification::from_row).collect())
}

#[derive(Clone, Copy)]
pub(crate) enum NotificationSelection<'a> { Ids(&'a Vec<i64>), Group(i32), All }

pub(crate) async fn process_notifications(ex: &impl Executor, me: i64, selection: NotificationSelection<'_>) -> Result<u64, Error> {
    match selection {
        NotificationSelection::Ids(ids) => ex.execute("UPDATE game.notifications SET processed = true WHERE receiver_id = $1 AND notification_id = ANY($2)", &[&me, ids]).await,
        NotificationSelection::Group(group) => ex.execute("UPDATE game.notifications SET processed = true WHERE receiver_id = $1 AND group_id = $2", &[&me, &group]).await,
        NotificationSelection::All => ex.execute("UPDATE game.notifications SET processed = true WHERE receiver_id = $1", &[&me]).await,
    }
}
pub(crate) async fn delete_notifications(ex: &impl Executor, me: i64, selection: NotificationSelection<'_>) -> Result<u64, Error> {
    match selection {
        NotificationSelection::Ids(ids) => ex.execute("DELETE FROM game.notifications WHERE receiver_id = $1 AND notification_id = ANY($2)", &[&me, ids]).await,
        NotificationSelection::Group(group) => ex.execute("DELETE FROM game.notifications WHERE receiver_id = $1 AND group_id = $2", &[&me, &group]).await,
        NotificationSelection::All => ex.execute("DELETE FROM game.notifications WHERE receiver_id = $1", &[&me]).await,
    }
}
