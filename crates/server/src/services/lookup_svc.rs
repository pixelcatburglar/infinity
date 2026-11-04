//! War opponent lookup for `lookupSvc`, used by `war_cso.py` and warDeclare.
//! Searches live `game.corporations` and `game.alliances`; names are server-owned.
//! Structures and aggressive war permits are not supported.

use crate::{client::Client, consts::{TYPE_ALLIANCE, TYPE_CORPORATION}, dao, messages};
use crate::services::{Call, CallResult, db_err, keyval, notify_error};
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.method != "LookupWarableCorporationsOrAlliances" { return None; }
    Some(async {
        if call.args.len() != 2 { return Err(notify_error(messages::wars::INVALID)); }
        let text = call.args.first().and_then(Value::as_text).ok_or_else(|| notify_error(messages::wars::INVALID))?;
        let exact = match call.args.get(1) {
            Some(Value::Bool(value)) => *value,
            _ => return Err(notify_error(messages::wars::INVALID)),
        };
        let text = text.trim();
        if text.is_empty() { return Err(notify_error(messages::wars::INVALID)); }
        let corps = dao::corporations::wars::search(&c.db, text, exact).await.map_err(db_err)?;
        let alliances = dao::alliances::wars::search(&c.db, text, exact).await.map_err(db_err)?;
        let row = |id, name: String, type_id| keyval(vec![
            ("ownerID", Value::Int(id)), ("name", Value::unicode(name)),
            ("typeID", Value::Int(type_id)), ("warPermit", Value::Bool(false)),
        ]);
        Ok(Value::list(corps.into_iter().map(|r| row(r.id, r.name, TYPE_CORPORATION))
            .chain(alliances.into_iter().map(|r| row(r.id, r.name, TYPE_ALLIANCE)))))
    }.await)
}
