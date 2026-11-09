//! Client telemetry from publicGateway/grpc/eventPublisher.py:publish_event_payload.
//! Uses embedded protobuf descriptors only; telemetry is untrusted and no event
//! consumer or progress mutation is implemented.
use prost_reflect::{DynamicMessage, ReflectMessage};
use tracing::debug;
use infinity_core::consts::UUID_LEN;
use super::message::{message, type_name};

pub(super) fn receive(bytes: &[u8], application: &[u8; UUID_LEN]) -> Result<(), ()> {
    let descriptor = message("eve_public.Event").ok_or(())?;
    let event = DynamicMessage::decode(descriptor, bytes).map_err(|_| ())?;
    let field_bytes = |name| event.get_field_by_name(name).and_then(|v| v.as_bytes().cloned()).ok_or(());
    if field_bytes("uuid")?.len() != UUID_LEN || field_bytes("application_instance_uuid")?.as_ref() != application {
        return Err(());
    }
    let journey = field_bytes("journey")?;
    if !journey.is_empty() && journey.len() != UUID_LEN { return Err(()); }
    if !event.has_field_by_name("payload") { return Err(()); }
    let payload = event.get_field_by_name("payload").and_then(|v| v.as_message().cloned()).ok_or(())?;
    let url = payload.get_field_by_name("type_url").and_then(|v| v.as_str().map(str::to_owned)).ok_or(())?;
    let value = payload.get_field_by_name("value").and_then(|v| v.as_bytes().cloned()).ok_or(())?;
    if url.is_empty() { return Err(()); }
    if let Some(desc) = message(type_name(&url)) {
        let decoded = DynamicMessage::decode(desc, value.as_ref()).map_err(|_| ())?;
        debug!(event_type = decoded.descriptor().full_name(), "gateway: unclaimed event");
    } else {
        debug!(type_url = %url, "gateway: unclaimed unknown event");
    }
    Ok(())
}

#[cfg(test)] mod tests;
