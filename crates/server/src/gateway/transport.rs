//! HTTP/2 streams and gRPC framing for requestBroker.py and eventPublisher.py.
//! Uses gateway handlers and embedded descriptors; client events cannot change game state.
use anyhow::Result;
use bytes::{Buf, Bytes, BytesMut};
use prost::Message;
use std::sync::Arc;
use tracing::{debug, warn};
use infinity_core::consts::UUID_LEN;
use super::{Gateway, events, protocol::Request, requests::handle_request};

/// HTTP/2's 31-bit flow-control window limit (RFC 9113 §6.9.1); h2 0.4.20
/// casts reservations to WindowSize (share.rs) and uses this maximum (proto/mod.rs).
const H2_MAX_WINDOW_RESERVATION: usize = (1 << 31) - 1;
/// gRPC compression flag and four-byte message length.
const GRPC_HEADER_LEN: usize = 5;


// gRPC status codes: https://grpc.github.io/grpc/core/md_doc_statuscodes.html
const GRPC_OK: &str = "0";
const GRPC_INVALID_ARGUMENT: &str = "3";
const GRPC_UNIMPLEMENTED: &str = "12";

pub(super) fn grpc_frame(msg: &[u8]) -> Bytes {
    grpc_frame_encoded(msg.len(), |b| b.extend_from_slice(msg))
}

pub(super) fn grpc_frame_encoded(len: usize, encode: impl FnOnce(&mut BytesMut)) -> Bytes {
    let mut b = BytesMut::with_capacity(GRPC_HEADER_LEN + len);
    b.extend_from_slice(&[0]);
    b.extend_from_slice(&u32::try_from(len).expect("gRPC message exceeds wire limit").to_be_bytes());
    encode(&mut b);
    b.freeze()
}

fn checked_messages(buf: &mut BytesMut) -> Result<Vec<Bytes>, ()> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        if buf[0] != 0 { return Err(()); }
        if buf.len() < GRPC_HEADER_LEN { break; }
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if buf.len() - GRPC_HEADER_LEN < len { break; }
        buf.advance(GRPC_HEADER_LEN);
        out.push(buf.split_to(len).freeze());
    }
    Ok(out)
}

fn application_uuid(headers: &http::HeaderMap) -> Option<[u8; UUID_LEN]> {
    crate::uuid_text::parse(headers.get("application_instance_uuid")?.to_str().ok()?, crate::uuid_text::Case::Lowercase)
}

fn ok_headers() -> http::Response<()> {
    http::Response::builder().status(http::StatusCode::OK).header("content-type", "application/grpc").body(()).unwrap()
}
fn trailers(status: &'static str) -> http::HeaderMap {
    let mut t = http::HeaderMap::new();
    t.insert("grpc-status", http::HeaderValue::from_static(status));
    t
}
pub(super) async fn connection(sock: tokio::net::TcpStream, gw: Arc<Gateway>) -> Result<()> {
    let mut h2 = h2::server::handshake(sock).await?;
    while let Some(req) = h2.accept().await {
        let (req, respond) = req?;
        let gw = gw.clone();
        tokio::spawn(async move {
            let path = req.uri().path().to_string();
            let application = application_uuid(req.headers());
            if let Err(e) = stream(path.clone(), application, req.into_body(), respond, gw).await {
                debug!("gateway stream {path} ended: {e:#}");
            }
        });
    }
    Ok(())
}

async fn stream(path: String, application: Option<[u8; UUID_LEN]>, mut body: h2::RecvStream, mut respond: h2::server::SendResponse<Bytes>, gw: Arc<Gateway>) -> Result<()> {
    let mut send = respond.send_response(ok_headers(), false)?;
    let mut buf = BytesMut::new();
    match path.as_str() {
        "/eve_public.gateway.Requests/Send" | "/eve_public.gateway.Events/Publish" => {
            let Some(application) = application else { send.send_trailers(trailers(GRPC_INVALID_ARGUMENT))?; return Ok(()); };
            let lease = gw.notice_hub.lease(application);
            let requests = path == "/eve_public.gateway.Requests/Send";
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
                buf.extend_from_slice(&chunk);
                let messages = match checked_messages(&mut buf) {
                    Ok(messages) => messages,
                    Err(()) => { send.send_trailers(trailers(GRPC_INVALID_ARGUMENT))?; return Ok(()); }
                };
                for msg in messages {
                    if requests {
                        let req = match Request::decode(msg) {
                            Ok(req) if req.correlation_uuid.len() == UUID_LEN && req.application_instance_uuid.as_slice() == application => req,
                            _ => { send.send_trailers(trailers(GRPC_INVALID_ARGUMENT))?; return Ok(()); }
                        };
                        let rsp = handle_request(&gw, req, Some(&lease)).await;
                        send.send_data(grpc_frame(&rsp.encode_to_vec()), false)?;
                    } else if events::receive(&msg, &application).is_err() {
                        send.send_trailers(trailers(GRPC_INVALID_ARGUMENT))?;
                        return Ok(());
                    }
                }
            }
            send.send_trailers(trailers(if buf.is_empty() { GRPC_OK } else { GRPC_INVALID_ARGUMENT }))?;
        }
        "/eve_public.gateway.Notices/Consume" => {
            let Some(application) = application else { send.send_trailers(trailers(GRPC_INVALID_ARGUMENT))?; return Ok(()) };
            let mut registration = gw.notice_hub.register(application, gw.config.gateway.notice_queue_capacity);
            let mut pending: Option<Bytes> = None;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut registration.cancelled => return Ok(()),
                    chunk = body.data() => {
                        let Some(chunk) = chunk else { break };
                        let chunk = chunk?;
                        body.flow_control().release_capacity(chunk.len()).ok();
                        buf.extend_from_slice(&chunk);
                        if checked_messages(&mut buf).is_err() {
                            send.send_trailers(trailers(GRPC_INVALID_ARGUMENT))?;
                            return Ok(());
                        }
                    }
                    capacity = std::future::poll_fn(|cx| send.poll_capacity(cx)), if pending.is_some() => {
                        let Some(capacity) = capacity else { return Ok(()) };
                        let capacity = capacity?;
                        let frame = pending.as_mut().unwrap();
                        let chunk = frame.split_to(capacity.min(frame.len()));
                        if !chunk.is_empty() { send.send_data(chunk, false)?; }
                        if frame.is_empty() { pending = None; }
                        else { send.reserve_capacity(frame.len().min(H2_MAX_WINDOW_RESERVATION)); }
                    }
                    frame = registration.rx.recv(), if pending.is_none() => {
                        let Some(frame) = frame else { break };
                        send.reserve_capacity(frame.len().min(H2_MAX_WINDOW_RESERVATION));
                        pending = Some(frame);
                    }
                }
            }
            send.send_trailers(trailers(if buf.is_empty() { GRPC_OK } else { GRPC_INVALID_ARGUMENT }))?;
        }
        "/eve_public.gateway.Requests/Ping" | "/eve_public.gateway.Notices/Ping" | "/eve_public.gateway.Events/Ping" => {
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
            }
            send.send_data(grpc_frame(&[]), false)?;
            send.send_trailers(trailers(GRPC_OK))?;
        }
        other => {
            warn!("gateway: unknown gRPC method {other}");
            send.send_trailers(trailers(GRPC_UNIMPLEMENTED))?;
        }
    }
    Ok(())
}

#[cfg(test)] mod tests;
