//! Ephemeral post-commit gateway notices; no durable replay or World back-reference.
//! The client derives its tenant from token, startup, server and preferences in
//! monolithconfig/__init__.py:328-344, normalizes it in publicGatewaySvc.py:69-74,
//! and noticeConsumer.py:92-103,135-152 does not compare notice tenants.
//! Routing resolves one current in-memory pilot per stream per notice; this is a
//! deliberate linear cost, with no database access or locks held across awaits.

pub(super) mod registry;
pub(crate) use registry::NoticeHub;

use std::sync::Arc;
use prost::Message;
use prost_reflect::{DynamicMessage, ReflectMessage};
use super::{error::GatewayError, message, protocol::{self, target_group::Target}};

/// Server design name for its single deployment.
const DEPLOYMENT_TENANT: &str = "infinity";

/// Protobuf signed seconds and nanos varints can each occupy ten bytes; allow
/// this conservative additional growth when the dispatch timestamp changes.
const TIMESTAMP_ENCODED_GROWTH_BUDGET: usize = 20;

fn validate_target(target: &Target) -> Result<(), GatewayError> {
    if let Target::ApplicationInstanceUuid(uuid) = target {
        if uuid.len() != message::UUID_LEN {
            return Err(GatewayError::Internal(anyhow::anyhow!("invalid gateway notice application UUID length")));
        }
    }
    Ok(())
}

pub(crate) struct GatewayNotice {
    target: Target,
    uuid: Arc<[u8]>,
    payload: Arc<[u8]>,
}

impl GatewayNotice {
    pub(crate) fn new(target: Target, payload: &DynamicMessage) -> Result<Self, GatewayError> {
        validate_target(&target)?;
        let descriptor = payload.descriptor();
        if message::message(descriptor.full_name()).as_ref() != Some(&descriptor) {
            return Err(GatewayError::Internal(anyhow::anyhow!("noncanonical gateway notice descriptor {}", descriptor.full_name())));
        }
        let mut value = Vec::with_capacity(payload.encoded_len());
        payload.encode(&mut value).map_err(|e| GatewayError::Internal(e.into()))?;
        let payload = prost_types::Any { type_url: format!("type.googleapis.com/{}", descriptor.full_name()), value }.encode_to_vec();
        let uuid = message::new_uuid();
        let envelope = protocol::Notice {
            dispatched: Some(protocol::now_timestamp()),
            uuid: uuid.clone(), internal_origin: DEPLOYMENT_TENANT.into(), tenant: DEPLOYMENT_TENANT.into(),
            payload: None,
            target_group: Some(protocol::TargetGroup { target: Some(Target::ApplicationInstanceUuid(vec![0; 16])) }),
        };
        let len = envelope.encoded_len_with_payload(&payload).checked_add(TIMESTAMP_ENCODED_GROWTH_BUDGET)
            .ok_or_else(|| GatewayError::Internal(anyhow::anyhow!("gateway notice exceeds gRPC frame limit")))?;
        u32::try_from(len).map_err(|_| GatewayError::Internal(anyhow::anyhow!("gateway notice exceeds gRPC frame limit")))?;
        Ok(Self { target, uuid: uuid.into(), payload: payload.into() })
    }

    pub(crate) fn retarget(&self, target: Target) -> Result<Self, GatewayError> {
        validate_target(&target)?;
        Ok(Self { target, uuid: self.uuid.clone(), payload: self.payload.clone() })
    }

    fn frame(&self) -> bytes::Bytes {
        let notice = protocol::Notice { dispatched: Some(protocol::now_timestamp()), uuid: self.uuid.to_vec(), internal_origin: DEPLOYMENT_TENANT.into(), tenant: DEPLOYMENT_TENANT.into(), payload: None, target_group: Some(protocol::TargetGroup { target: Some(self.target.clone()) }) };
        super::transport::grpc_frame_encoded(notice.encoded_len_with_payload(&self.payload), |buf| notice.encode_with_payload(&self.payload, buf))
    }
}

/// Converts producer construction faults to the existing internal service error.
pub(crate) fn producer_error(error: GatewayError) -> crate::services::CallError {
    tracing::warn!(?error, "gateway notice construction failed");
    crate::services::CallError::Internal(anyhow::anyhow!("gateway notice construction failed"))
}

#[cfg(test)]
mod tests;
