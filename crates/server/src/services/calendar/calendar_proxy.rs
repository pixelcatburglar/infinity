//! Calendar proxy for `eve/client/script/ui/services/eveCalendarsvc.py`.
//! Reads `game.calendar_events` and `game.calendar_responses`; alliance events are unsupported.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match call.method {
        "GetEventList" => {
            let me = c.session.int("charid").unwrap_or(0);
            let corp = c.session.int("corpid").unwrap_or(0);
            match dao::event_list(&c.db, me, corp, crate::services::arg_int(call, 0) as i32, crate::services::arg_int(call, 1) as i32, RESPONSE_DELETED, FILETIME_UNIX_EPOCH, FILETIME_SECOND).await {
                Ok(rows) => {
                    let header = DbRowDescriptor::new([
                        ("eventID", DbType::I4), ("ownerID", DbType::I4), ("eventDateTime", DbType::FileTime), ("dateModified", DbType::FileTime),
                        ("eventDuration", DbType::I4), ("importance", DbType::I4), ("eventTitle", DbType::WStr), ("isDeleted", DbType::Bool),
                    ]);
                    let lines: Vec<Value> = rows.iter().map(|r| header.row(&[Value::Int(r.event_id), Value::Int(r.owner_id), Value::Int(r.event_date), Value::Int(r.modified), Value::Int(r.duration), Value::Int(r.importance), Value::unicode(r.title.as_str()), Value::Bool(r.deleted)])).collect();
                    Ok(Value::list([Value::List(lines)]))
                }
                Err(e) => Err(db_err(e)),
            }
        }
        "GetEventDetails" => match dao::details(&c.db, crate::services::arg_int(call, 0)).await {
            Ok(Some(r)) => Ok(keyval([("eventID", Value::Int(crate::services::arg_int(call, 0))), ("eventText", Value::unicode(r.description)), ("creatorID", Value::Int(r.creator_id)), ("ownerID", Value::Int(r.owner_id))])),
            Ok(None) => Ok(Value::None),
            Err(e) => Err(db_err(e)),
        },
        _ => return None,
    })
}
