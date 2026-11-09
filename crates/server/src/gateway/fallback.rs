//! Populate unclaimed responses for `requestBroker.py:_send_request`.
//! Uses request and descriptor fields without World or DAO access; unclaimed
//! requests with a `no_local_version_available` field instead have no payload.
use std::collections::HashMap;
use bytes::Bytes;
use prost_reflect::{DynamicMessage, ReflectMessage};
use infinity_core::consts::UUID_LEN;
use super::{Caller, message::message};

/// Server limit for nesting when collecting request identifiers.
const IDENTIFIER_DEPTH: u32 = 2;
/// Identifier-like sub-messages of a request (and the caller's character), by type,
/// so a default response can echo what it was asked about.
pub(super) fn known_ids(req: &DynamicMessage, caller: &Caller) -> HashMap<String, DynamicMessage> {
    use prost_reflect::Value as PV;
    let mut ids = HashMap::new();
    if let (Some(ch), Some(d)) = (caller.character_id, message("eve_public.character.Identifier")) {
        let mut m = DynamicMessage::new(d);
        if m.try_set_field_by_name("sequential", PV::U32(ch as u32)).is_ok() {
            ids.insert(m.descriptor().full_name().to_string(), m);
        }
    }
    fn walk(m: &DynamicMessage, ids: &mut HashMap<String, DynamicMessage>, depth: u32) {
        for (f, v) in m.fields() {
            if let PV::Message(sub) = v {
                if f.is_list() || f.is_map() {
                    continue;
                }
                if sub.descriptor().full_name().ends_with(".Identifier") {
                    ids.insert(sub.descriptor().full_name().to_string(), sub.clone());
                } else if depth > 0 {
                    walk(sub, ids, depth - 1);
                }
            }
        }
    }
    walk(req, &mut ids, IDENTIFIER_DEPTH);
    ids
}

/// Make a default response safe to read:
/// * singular sub-messages are populated (echoing identifiers from `ids`),
/// * `uuid` byte fields get the all-zero UUID (the client's "none", e.g. skill plans),
/// * `eve_public.math.Fraction`s are 0/1 rather than 0/0,
/// * a oneof offering a "nothing" flag (`no_title`, `no_equipped`, `not_completed`,
///   `none`...) has it set, which is how these APIs say "absent".
pub(super) fn fill_defaults(m: &mut DynamicMessage, depth: u32, ids: &HashMap<String, DynamicMessage>) {
    use prost_reflect::{Kind, Value as PV};
    if depth == 0 {
        return;
    }
    let desc = m.descriptor();
    if desc.full_name() == "eve_public.math.Fraction" {
        let _ = m.try_set_field_by_name("denominator", PV::U64(1));
        return;
    }
    for o in desc.oneofs() {
        if o.fields().any(|f| m.has_field(&f)) {
            continue;
        }
        let nothing = o.fields().find(|f| {
            let n = f.name();
            matches!(f.kind(), Kind::Bool) && (n.starts_with("no_") || n.starts_with("not_") || n == "none")
        });
        if let Some(f) = nothing {
            m.set_field(&f, PV::Bool(true));
        }
    }
    for f in desc.fields() {
        if f.is_list() || f.is_map() || f.containing_oneof().is_some() {
            continue;
        }
        match f.kind() {
            Kind::Bytes if f.name() == "uuid" => m.set_field(&f, PV::Bytes(Bytes::from_static(&[0u8; UUID_LEN]))),
            Kind::Message(md) if !md.full_name().starts_with("google.protobuf.") => {
                if let Some(id) = ids.get(md.full_name()) {
                    m.set_field(&f, PV::Message(id.clone()));
                    continue;
                }
                let mut sub = DynamicMessage::new(md);
                fill_defaults(&mut sub, depth - 1, ids);
                m.set_field(&f, PV::Message(sub));
            }
            _ => {}
        }
    }
}

#[cfg(test)] mod tests;
