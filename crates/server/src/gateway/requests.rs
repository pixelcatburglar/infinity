//! Decode and route requests from `requestBroker.py:new_eve_public_request`.
//! Uses embedded descriptors and gateway handlers, not World; unclaimed CDN
//! requests have no payload and other unclaimed requests receive defaults.
use prost_reflect::{DynamicMessage, ReflectMessage};
use tracing::{debug, info, warn};
use super::{Gateway, Caller, handlers, protocol::{Request, Response, now_timestamp}, message::{message, pack, response_name, type_name}, fallback::{known_ids, fill_defaults}};

/// Server limit for gateway request log excerpts, in bytes.
const LOG_EXCERPT_LEN: usize = 300;
/// Server limit for nesting when populating default gateway responses.
const DEFAULT_RESPONSE_DEPTH: u32 = 4;
pub(super) async fn handle_request(gw: &Gateway, req: Request) -> Response {
    let ctx = req.authoritative_context.clone().unwrap_or_default();
    let caller = Caller {
        user_id: ctx.authenticated_user.map(|u| u.sequential as i64),
        character_id: ctx.identity.and_then(|i| i.character).or(ctx.active_character).map(|c| c.sequential as i64),
    };
    gw.identities.lock().unwrap().insert(req.application_instance_uuid.clone(), (caller.user_id, caller.character_id));
    let mut rsp = Response {
        dispatched: Some(now_timestamp()),
        correlation_uuid: req.correlation_uuid.clone(),
        status_code: http::StatusCode::OK.as_u16() as u32,
        status_message: String::new(),
        payload: None,
        internal_origin: "infinity".into(),
        application_instance_uuid: req.application_instance_uuid.clone(),
    };
    let Some(any) = req.payload else {
        rsp.status_code = http::StatusCode::BAD_REQUEST.as_u16() as u32;
        rsp.status_message = "missing payload".into();
        return rsp;
    };
    let name = type_name(&any.type_url).to_string();
    let Some(desc) = message(&name) else {
        warn!("gateway: unknown request type {name}");
        rsp.status_code = http::StatusCode::BAD_REQUEST.as_u16() as u32;
        rsp.status_message = format!("unknown type {name}");
        return rsp;
    };
    let payload = match DynamicMessage::decode(desc, any.value.as_slice()) {
        Ok(m) => m,
        Err(e) => {
            rsp.status_code = http::StatusCode::BAD_REQUEST.as_u16() as u32;
            rsp.status_message = format!("bad payload: {e}");
            return rsp;
        }
    };
    match handlers::handle(gw, &caller, &name, &payload).await {
        Ok(Some(out)) => {
            info!("gateway: {name} (char {:?}) -> {}", caller.character_id, out.descriptor().full_name());
            rsp.payload = Some(pack(&out));
        }
        Ok(None) if payload.descriptor().get_field_by_name("no_local_version_available").is_some() => {
            // CDN content checkpoints (content/client/flatbuffer/provider.py): no payload
            // means "nothing published", which the client treats as no content.
            debug!("gateway: no content for {name}");
        }
        Ok(None) => {
            // Unimplemented: answer with the matching `...Response` so the UI can proceed.
            let rsp_name = response_name(&name);
            match rsp_name.as_deref().and_then(message) {
                Some(d) => {
                    info!("gateway: default reply {} (char {:?}) for {name} {}", d.full_name(), caller.character_id, short_msg(&payload));
                    let mut m = DynamicMessage::new(d);
                    fill_defaults(&mut m, DEFAULT_RESPONSE_DEPTH, &known_ids(&payload, &caller));
                    rsp.payload = Some(pack(&m));
                }
                None => {
                    warn!("gateway: UNHANDLED {name} (no matching response type) {}", short_msg(&payload));
                    rsp.status_code = http::StatusCode::NOT_IMPLEMENTED.as_u16() as u32;
                    rsp.status_message = "not implemented".into();
                }
            }
        }
        Err(e) => {
            warn!("gateway: {name} failed: {e:#}");
            rsp.status_code = http::StatusCode::INTERNAL_SERVER_ERROR.as_u16() as u32;
            rsp.status_message = format!("{e:#}");
        }
    }
    rsp
}
fn short_msg(m: &DynamicMessage) -> String {
    let s = format!("{m}");
    if s.len() > LOG_EXCERPT_LEN {
        format!("{}...", &s[..s.char_indices().take_while(|(i, _)| *i < LOG_EXCERPT_LEN).last().map(|(i, _)| i).unwrap_or(0)])
    } else {
        s
    }
}
