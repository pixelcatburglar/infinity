//! standingMgr answers standingsvc using game.standings and game.standing_history;
//! corporation standings are not modelled.
//!
//! `GetStandingTransactions(fromID, toID)` returns the character's journal rows
//! for one owner pair, newest first, as the keyval records
//! `eve/common/script/util/eveFormat.py:FmtStandingTransaction` reads. The client
//! multiplies `modification` by the standing limit back into absolute units.
use crate::services::{db_err, keyval, notify_error, rowset, Call, CallResult};
use crate::client::Client;
use infinity_core::messages;
use marshal::Value;

/// Standings panel history query: exactly `fromID` and `toID` as positive
/// integers, and the client only ever asks for its own character's rows.
async fn transactions(c: &mut Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let (from, to) = match call.args {
        [Value::Int(from), Value::Int(to)] if *from > 0 && *to > 0 => (*from, *to),
        _ => return Err(notify_error(messages::standings::HISTORY_INVALID)),
    };
    if to != me {
        return Err(notify_error(messages::standings::HISTORY_INVALID));
    }
    let rows = crate::dao::characters::standings::history::list(&c.db, me, from).await.map_err(db_err)?;
    Ok(Value::list(rows.iter().map(|row| {
        // int_1 by eventTypeID (eveFormat.py): the GM actor on a slash set, the
        // mission name localization ID on a mission event, None otherwise —
        // including the script actor, which the wire does not expose.
        let int_1 = match row.event_type_id {
            t if t == super::mutation::EVENT_SLASH_SET => row.actor_id,
            t if t == super::mutation::EVENT_MISSION_COMPLETED || t == super::mutation::EVENT_MISSION_DECLINED || t == super::mutation::EVENT_MISSION_FAILED => row.mission_name_id,
            _ => None,
        }.map(Value::Int).unwrap_or(Value::None);
        keyval([
            ("eventID", Value::Int(row.event_id)),
            ("fromID", Value::Int(row.from_id)),
            ("toID", Value::Int(me)),
            ("eventTypeID", Value::Int(row.event_type_id)),
            ("eventDateTime", Value::Int(row.occurred_at)),
            ("modification", Value::Float((row.new_standing - row.old_standing) / super::STANDING_LIMIT)),
            ("msg", Value::unicode(row.message.as_str())),
            ("int_1", int_1),
            ("int_2", Value::None),
            ("int_3", Value::None),
        ])
    })))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    Some(match call.method {
        "GetCharStandings" => match crate::dao::characters::standings::list(&c.db, me).await {
            Ok(rows) => Ok(rowset(&["fromID", "standing"], rows.iter().map(|r| vec![Value::Int(r.from_id), Value::Float(r.standing)]).collect())),
            Err(e) => Err(db_err(e)),
        },
        "GetCorpStandings" => Ok(rowset(&["fromID", "standing"], vec![])),
        "GetStandingTransactions" => transactions(c, call).await,
        "GetStandingCompositions" => Ok(Value::list([])),
        _ => return None,
    })
}
