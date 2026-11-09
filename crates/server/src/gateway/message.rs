//! Descriptor and message helpers for `requestBroker.py:_send_request` payloads.
//! Uses embedded descriptors and server-generated UUIDs, not World or DAO data;
//! descriptor lookup and field helpers do not implement missing services.
use anyhow::{Context, Result};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, ReflectMessage, Value as PV};
use std::sync::OnceLock;
use tracing::warn;

/// RFC 4122 version field in the seventh UUID byte.
pub(super) const UUID_VERSION_INDEX: usize = 6;
/// RFC 4122 variant field in the ninth UUID byte.
pub(super) const UUID_VARIANT_INDEX: usize = 8;
/// RFC 4122 version field payload mask.
pub(super) const UUID_VERSION_MASK: u8 = 0x0f;
/// RFC 4122 random UUID version.
pub(super) const UUID_VERSION_V4: u8 = 0x40;
/// RFC 4122 variant field payload mask.
pub(super) const UUID_VARIANT_MASK: u8 = 0x3f;
/// RFC 4122 variant bits.
pub(super) const UUID_VARIANT_RFC4122: u8 = 0x80;
static DESCRIPTORS: &[u8] = include_bytes!("../../proto/eve_protos.desc");

pub(crate) fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        // The client ships a few conflicting duplicates (e.g. two files defining
        // eve.payment.customer.Identifier), so add files one at a time in dependency
        // order and skip the ones that cannot be added.
        let set = prost_types::FileDescriptorSet::decode(DESCRIPTORS).expect("client protobuf descriptor set");
        let mut pool = DescriptorPool::new();
        let mut pending = set.file;
        loop {
            let before = pending.len();
            let mut next = Vec::new();
            for fd in pending {
                let ready = fd.dependency.iter().all(|d| pool.get_file_by_name(d).is_some());
                if !ready {
                    next.push(fd);
                } else if let Err(e) = pool.add_file_descriptor_proto(fd.clone()) {
                    warn!("skipping proto {}: {e}", fd.name());
                }
            }
            pending = next;
            if pending.is_empty() || pending.len() == before {
                break;
            }
        }
        if !pending.is_empty() {
            warn!("{} proto files have unresolvable imports", pending.len());
        }
        pool
    })
}
pub(super) fn type_name(type_url: &str) -> &str {
    type_url.rsplit('/').next().unwrap_or(type_url)
}

pub(super) fn pack(msg: &DynamicMessage) -> prost_types::Any {
    prost_types::Any { type_url: format!("type.googleapis.com/{}", msg.descriptor().full_name()), value: msg.encode_to_vec() }
}

pub(crate) fn message(name: &str) -> Option<MessageDescriptor> {
    pool().get_message_by_name(name)
}

pub(super) fn response_name(request: &str) -> Option<String> {
    request.strip_suffix("Request").map(|s| format!("{s}Response"))
}
pub(super) fn msg_field(m: &DynamicMessage, field: &str) -> Option<DynamicMessage> {
    m.get_field_by_name(field)?.as_message().cloned()
}

pub(super) fn str_field(m: &DynamicMessage, field: &str) -> String {
    m.get_field_by_name(field).and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

/// `eve_public.<x>.Identifier{sequential}`
pub(crate) fn identifier(parent: &MessageDescriptor, field: &str, sequential: PV) -> Result<DynamicMessage> {
    let mut id = child(parent, field)?;
    set(&mut id, "sequential", sequential)?;
    Ok(id)
}
pub(super) fn new_uuid() -> Vec<u8> {
    let mut id = [crate::handshake::rand_i64().to_le_bytes(), crate::handshake::rand_i64().to_le_bytes()].concat();
    id[UUID_VERSION_INDEX] = (id[UUID_VERSION_INDEX] & UUID_VERSION_MASK) | UUID_VERSION_V4;
    id[UUID_VARIANT_INDEX] = (id[UUID_VARIANT_INDEX] & UUID_VARIANT_MASK) | UUID_VARIANT_RFC4122;
    id
}
pub(crate) fn response_for(request: &str) -> Result<DynamicMessage> {
    let name = response_name(request).context("not a request")?;
    Ok(DynamicMessage::new(message(&name).with_context(|| format!("no {name}"))?))
}

/// Message type of `field` in `desc` (singular, repeated or map value alike).
pub(crate) fn child(desc: &MessageDescriptor, field: &str) -> Result<DynamicMessage> {
    let f = desc.get_field_by_name(field).with_context(|| format!("{} has no {field}", desc.full_name()))?;
    Ok(DynamicMessage::new(f.kind().as_message().with_context(|| format!("{field} is not a message"))?.clone()))
}

pub(crate) fn set(m: &mut DynamicMessage, field: &str, v: PV) -> Result<()> {
    m.try_set_field_by_name(field, v).with_context(|| format!("setting {}.{field}", m.descriptor().full_name()))
}

#[cfg(test)] mod tests;
