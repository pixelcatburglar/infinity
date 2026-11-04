//! HTTP/2 streams and gRPC framing for requestBroker.py and eventPublisher.py.
//! Uses gateway handlers and embedded descriptors; notice delivery remains
//! unimplemented and client events cannot change game state.
use anyhow::Result;
use bytes::{Buf, Bytes, BytesMut};
use prost::Message;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use super::{Gateway, events, message::UUID_LEN, protocol::{Request, Notice}, requests::handle_request};

/// gRPC compression flag and four-byte message length.
const GRPC_HEADER_LEN: usize = 5;

// RFC 4122 section 3: UUID text uses 8-4-4-4-12 hexadecimal digits.
const UUID_TEXT_LEN: usize = 36;
const UUID_HYPHEN_POSITIONS: [usize; 4] = [8, 13, 18, 23];
const UUID_HEX_LETTER_OFFSET: u8 = 10;
const UUID_NIBBLE_BITS: u32 = 4;

// gRPC status codes: https://grpc.github.io/grpc/core/md_doc_statuscodes.html
const GRPC_OK: &str = "0";
const GRPC_INVALID_ARGUMENT: &str = "3";
const GRPC_UNIMPLEMENTED: &str = "12";

fn grpc_frame(msg: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(GRPC_HEADER_LEN + msg.len());
    b.extend_from_slice(&[0]);
    b.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    b.extend_from_slice(msg);
    b.freeze()
}

// Consume retains its existing permissive framing.
fn take_messages(buf: &mut BytesMut) -> Vec<Bytes> {
    let mut out = Vec::new();
    while buf.len() >= GRPC_HEADER_LEN {
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if buf.len() < GRPC_HEADER_LEN + len { break; }
        buf.advance(GRPC_HEADER_LEN);
        out.push(buf.split_to(len).freeze());
    }
    out
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
    let text = headers.get("application_instance_uuid")?.to_str().ok()?.as_bytes();
    if text.len() != UUID_TEXT_LEN || UUID_HYPHEN_POSITIONS.iter().any(|&i| text[i] != b'-') { return None; }
    let mut result = [0; UUID_LEN];
    let mut digits = 0;
    for (i, &c) in text.iter().enumerate() {
        if UUID_HYPHEN_POSITIONS.contains(&i) { continue; }
        let n = match c { b'0'..=b'9' => c - b'0', b'a'..=b'f' => c - b'a' + UUID_HEX_LETTER_OFFSET, _ => return None };
        if digits % 2 == 0 { result[digits / 2] = n << UUID_NIBBLE_BITS; } else { result[digits / 2] |= n; }
        digits += 1;
    }
    Some(result)
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
                        let rsp = handle_request(&gw, req).await;
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
            let (tx, mut rx) = mpsc::unbounded_channel::<Notice>();
            let mut app_uuid: Option<Vec<u8>> = None;
            loop {
                tokio::select! {
                    chunk = body.data() => {
                        let Some(chunk) = chunk else { break };
                        let chunk = chunk?;
                        body.flow_control().release_capacity(chunk.len()).ok();
                        buf.extend_from_slice(&chunk);
                        take_messages(&mut buf);
                        if app_uuid.is_none() {
                            let key = crate::handshake::rand_i64().to_le_bytes().to_vec();
                            gw.notice_streams.lock().unwrap().insert(key.clone(), tx.clone());
                            app_uuid = Some(key);
                        }
                    }
                    Some(n) = rx.recv() => { send.send_data(grpc_frame(&n.encode_to_vec()), false)?; }
                }
            }
            if let Some(k) = app_uuid { gw.notice_streams.lock().unwrap().remove(&k); }
            send.send_trailers(trailers(GRPC_OK))?;
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
