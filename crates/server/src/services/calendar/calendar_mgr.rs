//! Calendar manager for `eve/client/script/ui/services/eveCalendarsvc.py`.
//! Uses `game.calendar_events` and `game.calendar_responses`; alliance events are unsupported.
use super::*;

async fn create(c: &Client, call: &Call<'_>, owner: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let when = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let duration = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let policy = &crate::config::game().calendar;
    let (title, description) = (value_text(call.args.get(2), usize::try_from(policy.title_max_len).unwrap_or(usize::MAX)), value_text(call.args.get(3), usize::try_from(policy.description_max_len).unwrap_or(usize::MAX)));
    let important = call.args.get(4).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let now = filetime_now();
    let guests: Vec<i64> = call.args.get(5).and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).filter(|g| *g != me).take(usize::try_from(policy.max_invitees).unwrap_or(usize::MAX)).collect();
    let world = c.world.clone();
    c.db.transaction(
        |tx, outbox: &mut Outbox| {
            let title = title.clone();
            let description = description.clone();
            let guests = guests.clone();
            Box::pin(async move {
                if owner != me && !crate::dao::corporations::registry::lock_admission(tx, &[], &[owner], &[me]).await.map_err(db_err)? {
                    return Err(notify_error(messages::PLAYER_CORP_DIRECTOR_REQUIRED_FOR_CORP_EVENT));
                }
                if !may_manage(tx, me, owner).await? { return Err(notify_error(messages::PLAYER_CORP_DIRECTOR_REQUIRED_FOR_CORP_EVENT)); }
                let id = dao::create(tx, &dao::NewEvent { owner, creator: me, when, duration, title: &title, description: &description, importance: important, modified: now }).await.map_err(db_err)?;
                if owner == me {
                    dao::initial_responses(tx, id, me, &guests, RESPONSE_UNDECIDED, RESPONSE_ACCEPTED).await.map_err(db_err)?;
                    for g in guests {
                        outbox.notify_pilot(g, "OnNewCalendarEvent", vec![Value::Int(id), Value::Int(owner), Value::Int(when), Value::Int(duration as i64), Value::unicode(title.as_str()), Value::Int(important as i64)]);
                    }
                }
                Ok(Value::Int(id))
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, &world) },
    ).await
}
async fn may_manage(ex: &(impl crate::db::executor::Executor + ?Sized), me: i64, owner: i64) -> Result<bool, CallError> {
    if owner == me { return Ok(true); }
    super::super::corps::permissions::has(ex, me, owner, ROLE_DIRECTOR).await
}
async fn edit(c: &Client, call: &Call<'_>) -> CallResult {
    let event = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let me = c.session.int("charid").unwrap_or(0);
    let Some(owner) = dao::owner(&c.db, event).await.map_err(db_err)? else { return Err(notify_error(messages::CALENDAR_EVENT_NOT_FOUND)); };
    if !may_manage(&c.db, me, owner).await? { return Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED)); }
    let old = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    let when = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(old);
    let duration = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let policy = &crate::config::game().calendar;
    let title = value_text(call.args.get(4), usize::try_from(policy.title_max_len).unwrap_or(usize::MAX));
    let important = call.args.get(6).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let description = value_text(call.args.get(5), usize::try_from(policy.description_max_len).unwrap_or(usize::MAX));
    let modified = filetime_now();
    c.db.transaction(
        |tx, outbox: &mut Outbox| {
            let title = title.clone();
            let description = description.clone();
            Box::pin(async move {
                if owner != me && !crate::dao::corporations::registry::lock_admission(tx, &[], &[owner], &[me]).await.map_err(db_err)? {
                    return Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED));
                }
                if dao::owner(tx, event).await.map_err(db_err)? != Some(owner) || !may_manage(tx, me, owner).await? {
                    return Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED));
                }
                dao::edit(tx, &dao::EditEvent { event, when, duration, title: &title, description: &description, importance: important, modified }).await.map_err(db_err)?;
                for g in dao::invitees(tx, event, RESPONSE_DELETED).await.map_err(db_err)? {
                    outbox.notify_pilot(g, "OnEditCalendarEvent", vec![Value::Int(event), Value::Int(owner), Value::Int(old), Value::Int(when), Value::Int(duration as i64), Value::unicode(title.as_str()), Value::Int(important as i64)]);
                }
                Ok(Value::None)
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, &c.world) },
    ).await
}

async fn delete(c: &Client, event: i64, me: i64) -> CallResult {
    let Some(found) = dao::owner_date(&c.db, event).await.map_err(db_err)? else { return Ok(Value::None) };
    let owner = found.owner_id;
    let now = filetime_now();
    c.db.transaction(
        |tx, outbox: &mut Outbox| Box::pin(async move {
            if owner != me && !crate::dao::corporations::registry::lock_admission(tx, &[], &[owner], &[me]).await.map_err(db_err)? {
                return Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED));
            }
            let Some(current) = dao::owner_date(tx, event).await.map_err(db_err)? else { return Ok(Value::None) };
            if current.owner_id != owner { return Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED)); }
            if may_manage(tx, me, owner).await? {
                let guests = dao::invitees(tx, event, RESPONSE_DELETED).await.map_err(db_err)?;
                dao::delete_event(tx, event, now).await.map_err(db_err)?;
                for guest in guests { outbox.notify_pilot(guest, "OnRemoveCalendarEvent", vec![Value::Int(event), Value::Int(current.event_date), Value::Bool(true)]); }
            } else {
                dao::delete_response(tx, event, me, RESPONSE_DELETED).await.map_err(db_err)?;
            }
            Ok(Value::None)
        }),
        |value, outbox| async move { dispatch_world(value, outbox, &c.world) },
    ).await
}
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "CreatePersonalEvent" => create(c, call, me).await,
        "CreateCorporationEvent" => {
            let corp = c.session.int("corpid").unwrap_or(0);
            if !super::super::corps::is_player_corp(corp) { return Some(Err(notify_error(messages::PLAYER_CORP_DIRECTOR_REQUIRED_FOR_CORP_EVENT))); }
            create(c, call, corp).await
        }
        "EditPersonalEvent" | "EditCorporationEvent" => edit(c, call).await,
        "DeleteEvent" => delete(c, int(0), me).await,
        "SendEventResponse" => dao::set_response(&c.db, int(0), me, int(2) as i32).await.map(|_| Value::None).map_err(db_err),
        "GetResponsesForCharacter" => match dao::character_responses(&c.db, me).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| keyval([("eventID", Value::Int(r.event_id)), ("status", Value::Int(r.status))])))), Err(e) => Err(db_err(e)),
        },
        "GetResponsesToEvent" => match dao::event_responses(&c.db, int(0)).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| keyval([("characterID", Value::Int(r.char_id)), ("status", Value::Int(r.status))])))), Err(e) => Err(db_err(e)),
        },
        "UpdateEventParticipants" => {
            let event = int(0);
            let owner = match dao::owner(&c.db, event).await { Ok(r) => r, Err(e) => return Some(Err(db_err(e))) };
            if owner != Some(me) { return Some(Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED))); }
            for g in value_id_list(call.args.get(1)) {
                if let Err(e) = dao::add_response(&c.db, event, g, RESPONSE_UNDECIDED).await { return Some(Err(db_err(e))); }
            }
            let gone = value_id_list(call.args.get(2));
            dao::remove_responses(&c.db, event, &gone, me).await.map(|_| Value::None).map_err(db_err)
        }
        _ => return None,
    })
}
