//! Descriptor-driven adaptation snapshots and ordered client notices.
//! Persistent chip rows are overlaid on the retained immutable catalogue, including
//! zero balances. Unknown persisted types are treated as an invariant failure.

use crate::{dao::ship_adaption::Snapshot, lua::content::{adaption::Definitions, protocol::{self, Chip, Identity, Response}}, services::{CallError, db_err}};
use marshal::Value;

pub(crate) fn chips(definitions: &Definitions, snapshot: &Snapshot) -> Result<Vec<Chip>, CallError> {
    let mut result = Vec::with_capacity(definitions.chip_types().len());
    let mut rows = snapshot.chips.iter().peekable();
    for &type_id in definitions.chip_types() {
        if rows.peek().is_some_and(|row| row.type_id < type_id) {
            return Err(db_err(anyhow::anyhow!("persisted adaptation type absent from catalogue")));
        }
        let quantity = if rows.peek().is_some_and(|row| row.type_id == type_id) {
            rows.next().expect("peeked row").balance
        } else { 0 };
        result.push(Chip { type_id, quantity });
    }
    if rows.next().is_some() { return Err(db_err(anyhow::anyhow!("persisted adaptation type absent from catalogue"))); }
    Ok(result)
}

pub(crate) fn user(definitions: &Definitions, snapshot: &Snapshot, user: i64, character: i64) -> Result<String, CallError> {
    let chips = chips(definitions, snapshot)?;
    protocol::encode_content_text(Response::User { identity: Identity { user, character }, chips: &chips }).map_err(db_err)
}

pub(crate) fn notice<X>(outbox: &mut crate::db::outbox::Outbox<X>, event: &str, text: String) {
    outbox.notify_client(event, vec![Value::str(text)]);
}
