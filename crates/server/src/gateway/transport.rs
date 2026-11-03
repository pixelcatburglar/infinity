//! HTTP/2 streams and gRPC framing for `requestBroker.py:_send_request` and
//! `noticeConsumer.py:consume_notices`. Uses gateway state and request handlers,
//! not World directly; Events are drained and notice delivery is not repaired.
use anyhow::{Context, Result};
use bytes::{Buf, Bytes, BytesMut};
use prost::Message;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use super::{Gateway, protocol::{Request, Notice}, requests::handle_request};

/// gRPC protocol: one compression byte followed by a four-byte message length.
const GRPC_HEADER_LEN: usize = 5;
/// gRPC status code UNIMPLEMENTED, encoded as an ASCII trailer value.
const GRPC_UNIMPLEMENTED: &str = "12";
/// gRPC length-prefixed message framing.
fn grpc_frame(msg: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(GRPC_HEADER_LEN + msg.len());
    b.extend_from_slice(&[0]);
    b.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    b.extend_from_slice(msg);
    b.freeze()
}

/// Pull complete gRPC messages out of `buf`.
fn take_messages(buf: &mut BytesMut) -> Vec<Bytes> {
    let mut out = Vec::new();
    while buf.len() >= GRPC_HEADER_LEN {
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if buf.len() < GRPC_HEADER_LEN + len {
            break;
        }
        buf.advance(GRPC_HEADER_LEN);
        out.push(buf.split_to(len).freeze());
    }
    out
}

fn ok_headers() -> http::Response<()> {
    http::Response::builder().status(http::StatusCode::OK).header("content-type", "application/grpc").body(()).unwrap()
}

fn ok_trailers() -> http::HeaderMap {
    let mut t = http::HeaderMap::new();
    t.insert("grpc-status", http::HeaderValue::from_static("0"));
    t
}
pub(super) async fn connection(sock: tokio::net::TcpStream, gw: Arc<Gateway>) -> Result<()> {
    let mut h2 = h2::server::handshake(sock).await?;
    while let Some(req) = h2.accept().await {
        let (req, respond) = req?;
        let gw = gw.clone();
        tokio::spawn(async move {
            let path = req.uri().path().to_string();
            if let Err(e) = stream(path.clone(), req.into_body(), respond, gw).await {
                debug!("gateway stream {path} ended: {e:#}");
            }
        });
    }
    Ok(())
}

async fn stream(
    path: String,
    mut body: h2::RecvStream,
    mut respond: h2::server::SendResponse<Bytes>,
    gw: Arc<Gateway>,
) -> Result<()> {
    let mut send = respond.send_response(ok_headers(), false)?;
    let mut buf = BytesMut::new();
    match path.as_str() {
        "/eve_public.gateway.Requests/Send" => {
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
                buf.extend_from_slice(&chunk);
                for msg in take_messages(&mut buf) {
                    let req = Request::decode(msg).context("decoding eve_public.Request")?;
                    let rsp = handle_request(&gw, req).await;
                    send.send_data(grpc_frame(&rsp.encode_to_vec()), false)?;
                }
            }
            send.send_trailers(ok_trailers())?;
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
                        // The client's acks carry nothing we need; register the stream on first contact.
                        take_messages(&mut buf);
                        if app_uuid.is_none() {
                            let key = crate::handshake::rand_i64().to_le_bytes().to_vec();
                            gw.notice_streams.lock().unwrap().insert(key.clone(), tx.clone());
                            app_uuid = Some(key);
                        }
                    }
                    Some(n) = rx.recv() => {
                        send.send_data(grpc_frame(&n.encode_to_vec()), false)?;
                    }
                }
            }
            if let Some(k) = app_uuid {
                gw.notice_streams.lock().unwrap().remove(&k);
            }
            send.send_trailers(ok_trailers())?;
        }
        "/eve_public.gateway.Events/Publish" => {
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
            }
            send.send_trailers(ok_trailers())?;
        }
        // Ping on any service: Empty -> Empty.
        p if p.ends_with("/Ping") => {
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
            }
            send.send_data(grpc_frame(&[]), false)?;
            send.send_trailers(ok_trailers())?;
        }
        other => {
            warn!("gateway: unknown gRPC method {other}");
            let mut t = http::HeaderMap::new();
            t.insert("grpc-status", http::HeaderValue::from_static(GRPC_UNIMPLEMENTED));
            send.send_trailers(t)?;
        }
    }
    Ok(())
}

#[cfg(test)] mod tests;
