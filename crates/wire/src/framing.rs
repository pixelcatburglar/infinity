//! Packet socket framing used by `sendpacket` / `recvpacketoob` in the
//! client's blue.dll (stacklessio).
//!
//! Each packet is a little-endian u32 header followed by the body. The low 28
//! bits are the body length; the high bits are flags (carbonengine io
//! `protocol.h`). Little-endian was confirmed by disassembling the client's
//! parser, which loads the header with a plain `mov` and masks `0x0FFFFFFF`.
//! As server policy, zlib packets may inflate to at most the transport's
//! maximum packet size; larger decompressed packets are rejected.

use std::io::{self, Read};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Carbonengine io `protocol.h`: low bits containing packet length.
const SIZE_MASK: u32 = 0x0FFF_FFFF;
/// Carbonengine io `protocol.h`: body has a u32 OOB length and OOB bytes.
const FLAG_OOB: u32 = 1 << 28;
/// Carbonengine io `protocol.h`: body is zlib-compressed.
const FLAG_ZLIB: u32 = 1 << 29;
/// Carbonengine io `protocol.h`: body is snappy-compressed.
const FLAG_SNAPPY: u32 = 1 << 30;
/// Client SocketTransportFactory.MaxPacketSize, in bytes.
pub const MAX_PACKET: usize = 10 * 1024 * 1024;
/// Server policy: decompressed packets cannot exceed the transport packet limit.
const MAX_INFLATED_PACKET: usize = MAX_PACKET;
/// Carbonengine packet framing: a little-endian u32 occupies four bytes.
const HEADER_BYTES: usize = std::mem::size_of::<u32>();
/// Server allocation estimate for either form of zlib-compressed packet.
const INFLATE_CAPACITY_FACTOR: usize = 4;

pub fn inflate(payload: &[u8]) -> io::Result<Vec<u8>> {
    let capacity = payload.len().saturating_mul(INFLATE_CAPACITY_FACTOR).min(MAX_INFLATED_PACKET);
    let mut out = Vec::with_capacity(capacity);
    flate2::read::ZlibDecoder::new(payload)
        .take((MAX_INFLATED_PACKET + 1) as u64)
        .read_to_end(&mut out)?;
    if out.len() > MAX_INFLATED_PACKET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("inflated packet exceeds limit of {MAX_INFLATED_PACKET} bytes"),
        ));
    }
    Ok(out)
}

pub struct Frame {
    pub payload: Vec<u8>,
}

/// Read one packet; `Ok(None)` on clean EOF at a packet boundary.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut hdr = [0u8; HEADER_BYTES];
    match r.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let header = u32::from_le_bytes(hdr);
    let len = (header & SIZE_MASK) as usize;
    if len > MAX_PACKET {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("packet too large: {len} bytes")));
    }
    if header & FLAG_SNAPPY != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snappy-compressed packet"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;

    let mut payload = if header & FLAG_OOB != 0 && body.len() >= HEADER_BYTES {
        let n = u32::from_le_bytes(body[..HEADER_BYTES].try_into().unwrap()) as usize;
        if HEADER_BYTES + n > body.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "OOB length exceeds packet"));
        }
        body.drain(..HEADER_BYTES + n);
        body
    } else {
        body
    };
    if header & FLAG_ZLIB != 0 {
        payload = inflate(&payload)?;
    }
    Ok(Some(Frame { payload }))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > SIZE_MASK as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "packet too large"));
    }
    let mut buf = Vec::with_capacity(HEADER_BYTES + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf).await
}

#[cfg(test)] mod tests;
