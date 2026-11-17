//! Framed marshal transport and socket ownership for a client session.
//! Every complete frame write and the closing shutdown share one immutable
//! timeout; a timeout or write error marks the transport failed and nothing is
//! marshalled or written afterwards, so no frame can follow a partial write.
//! The close exception mirrors `carbon/common/script/net/GPSExceptions.py` and
//! `GPS.py`. No database state or cross-process session coordination lives here.
//! The default write bound is shared with server configuration; production still passes the configured value.

use crate::framing::{inflate, read_frame, write_frame};
use anyhow::{bail, Context, Result};
use marshal::Value;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::trace;

/// Server policy: default bound in seconds on one client or chat socket write.
pub const DEFAULT_CLIENT_WRITE_TIMEOUT_SECS: u64 = 30;
/// zlib CMF byte used by the client's MachoNet packet compression.
const ZLIB_HEADER: u8 = 0x78;
/// Server diagnostic policy: maximum packet bytes included in decode errors.
const DECODE_ERROR_PREVIEW_BYTES: usize = 64;

/// Bound one complete socket write; an elapsed deadline surfaces as `TimedOut`
/// and the underlying result passes through unchanged. The cancelled write may
/// have delivered a partial frame, so callers treat failure as terminal.
pub async fn write_with_timeout<F>(write: F, timeout: Duration) -> std::io::Result<()>
where
    F: Future<Output = std::io::Result<()>>,
{
    match tokio::time::timeout(timeout, write).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "socket write timed out")),
    }
}

/// Raw GPS close exception, not a MachoNet packet.
pub fn usurped_close() -> Value {
    let reason = Value::str(infinity_core::messages::SESSION_USURPED);
    Value::Reduce {
        data: Box::new(Value::tuple([
            Value::global("exceptions.GPSTransportClosed"),
            Value::tuple([reason.clone()]),
            Value::dict([
                ("reason", reason),
                ("reasonCode", Value::None),
                ("reasonArgs", Value::Dict(vec![])),
            ]),
        ])),
        list: vec![],
        dict: vec![],
    }
}

/// Raw refusal; ExceptionMappingGPCS.py:16 and machoNet.py:1664-1692 localize this code.
pub fn not_accepting_close(reason_code: &str) -> Value {
    Value::Reduce {
        data: Box::new(Value::tuple([
            Value::global("exceptions.GPSTransportClosed"),
            Value::tuple([Value::None]),
            Value::dict([
                ("reason", Value::None),
                ("reasonCode", Value::str(reason_code)),
                ("reasonArgs", Value::Dict(vec![])),
            ]),
        ])),
        list: vec![],
        dict: vec![],
    }
}

pub struct Conn {
    pub stream: TcpStream,
    pub peer: SocketAddr,
    write_timeout: Duration,
    write_failed: bool,
}

/// Read one marshalled value; `None` when the peer hung up.
async fn recv_value<R: AsyncRead + Unpin>(r: &mut R, peer: SocketAddr) -> Result<Option<Value>> {
    let Some(mut frame) = read_frame(r).await? else { return Ok(None) };
    // "Packet Compression: MachoNet": the transport zlib-compresses large packets
    // itself, without the framing flag. A marshal stream never starts with 'x'.
    if frame.payload.first() == Some(&ZLIB_HEADER) {
        frame.payload = inflate(&frame.payload).context("inflating machoNet-compressed packet")?;
    }
    let v = marshal::loads(&frame.payload)
        .with_context(|| format!("undecodable packet ({} bytes): {:02x?}", frame.payload.len(), &frame.payload[..frame.payload.len().min(DECODE_ERROR_PREVIEW_BYTES)]))?;
    trace!(target: "infinity_server::conn", %peer, "<< {v:?}");
    Ok(Some(v))
}

impl Conn {
    pub fn new(stream: TcpStream, peer: SocketAddr) -> Self {
        Conn { stream, peer, write_timeout: Duration::from_secs(DEFAULT_CLIENT_WRITE_TIMEOUT_SECS), write_failed: false }
    }

    /// Bind every write to `timeout`; production passes the configured value.
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    /// Next decoded value, or `None` when the client hung up.
    pub async fn recv(&mut self) -> Result<Option<Value>> {
        recv_value(&mut self.stream, self.peer).await
    }

    pub async fn send(&mut self, v: &Value) -> Result<()> {
        if self.write_failed {
            bail!("socket write already failed");
        }
        trace!(target: "infinity_server::conn", peer = %self.peer, ">> {v:?}");
        let result = write_with_timeout(write_frame(&mut self.stream, &marshal::dumps(v)), self.write_timeout).await;
        if result.is_err() {
            self.write_failed = true;
        }
        result?;
        Ok(())
    }

    /// Split into a writer and a background reader feeding a channel, so the
    /// session loop can also wait on server-side pushes.
    pub fn split(self) -> (Writer, mpsc::UnboundedReceiver<Result<Option<Value>>>) {
        let peer = self.peer;
        let (mut rd, wr) = self.stream.into_split();
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            loop {
                let r = recv_value(&mut rd, peer).await;
                let stop = !matches!(r, Ok(Some(_)));
                if tx.send(r).is_err() || stop {
                    break;
                }
            }
        });
        (Writer { half: wr, peer, reader: Some(reader), write_timeout: self.write_timeout, write_failed: self.write_failed }, rx)
    }
}

pub struct Writer {
    half: OwnedWriteHalf,
    pub peer: SocketAddr,
    reader: Option<JoinHandle<()>>,
    write_timeout: Duration,
    write_failed: bool,
}

impl Writer {
    pub async fn send(&mut self, v: &Value) -> Result<()> {
        if self.write_failed {
            bail!("socket write already failed");
        }
        trace!(target: "infinity_server::conn", peer = %self.peer, ">> {v:?}");
        let result = write_with_timeout(write_frame(&mut self.half, &marshal::dumps(v)), self.write_timeout).await;
        if result.is_err() {
            self.write_failed = true;
        }
        result?;
        Ok(())
    }

    /// Stop the reader before shutting down the write half and releasing ownership.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(reader) = self.reader.take() {
            reader.abort();
            let _ = reader.await;
        }
        // A failed write may have left a partial frame on the wire; the dropped
        // half closes the socket instead of appending anything after it.
        if self.write_failed {
            return Ok(());
        }
        let result = write_with_timeout(self.half.shutdown(), self.write_timeout).await;
        if result.is_err() {
            self.write_failed = true;
        }
        result?;
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(reader) = &self.reader {
            reader.abort();
        }
    }
}

#[cfg(test)]
mod tests;
