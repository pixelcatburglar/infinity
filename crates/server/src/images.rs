//! Image server: the client's `imageserverurl` (evePhotosvc RemoteImageCacher)
//! fetches `<Character|Corporation|Alliance|Type|Render>/<id>_<size>.<jpg|png>`.
//! Images live under a data directory; `PUT` stores one (used to publish the
//! portraits the client renders locally at character creation). It also serves
//! the content CDN under `/content/` (infinity_content::cdn).
//!
//! A size that isn't stored is served from the nearest stored size of the same image.
//! Images of things that exist on CCP's servers under the same IDs (NPC characters such
//! as agents, NPC corporations, factions, types) are fetched once from the upstream image
//! server (configured upstream) and kept. Player characters, corporations and alliances are
//! never fetched: their IDs are this server's own. Upload limits and upstream timeouts
//! are configured at startup.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

/// Server-supported image categories from the client image URL convention.
const KINDS: [&str; 5] = ["Character", "Corporation", "Alliance", "Type", "Render"];
/// Image sizes offered by the upstream image server, in pixels.
const SIZES: [u32; 6] = [32, 64, 128, 256, 512, 1024];
/// Server image policy: character and corporation IDs from here on are local.
const FIRST_PLAYER_ID: u64 = 90_000_000;
/// Upstream type icon maximum size, in pixels.
const MAX_TYPE_ICON_SIZE: u32 = 64;
/// Server policy: simultaneous connections on the image listener.
const IMAGE_MAX_CONNECTIONS: usize = 256;
/// Server policy: simultaneous image-listener connections from one address.
const IMAGE_MAX_CONNECTIONS_PER_ADDRESS: usize = 16;
/// Server policy: simultaneous connections on the content CDN listener.
const CDN_MAX_CONNECTIONS: usize = 256;
/// Server policy: simultaneous CDN connections from one address.
const CDN_MAX_CONNECTIONS_PER_ADDRESS: usize = 16;
/// Server policy: bound on one HTTP request line, terminator included.
const HTTP_REQUEST_LINE_LIMIT_BYTES: usize = 8192;
/// Server policy: bound on all HTTP headers, terminators and the final blank
/// line included.
const HTTP_HEADER_LIMIT_BYTES: usize = 32768;
/// Server policy: bound on reading the request line, headers and bounded body.
const HTTP_READ_TIMEOUT_SECS: u64 = 10;

/// Which listener accepted a connection; selects its log label and bounds.
#[derive(Clone, Copy)]
pub(crate) enum ListenerKind {
    Images,
    Cdn,
}

/// The upstream image server, if any.
#[derive(Clone)]
pub struct Upstream(Option<Arc<(String, reqwest::Client)>>);

impl Upstream {
    pub fn new(base: &str, timeout_secs: u64) -> Upstream {
        let base = base.trim_end_matches('/');
        if base.is_empty() {
            return Upstream(None);
        }
        let client = reqwest::Client::builder().user_agent("infinity-emulator image cache").timeout(std::time::Duration::from_secs(timeout_secs)).build();
        Upstream(client.ok().map(|c| Arc::new((base.to_string(), c))))
    }

    /// `<kind>/<id>_<size>.<ext>` on images.evetech.net, for images that are the same there.
    fn url(&self, kind: &str, id: u64, size: u32) -> Option<String> {
        let (base, _) = self.0.as_deref()?;
        let size = SIZES.into_iter().find(|s| *s >= size).unwrap_or(SIZES[SIZES.len() - 1]);
        Some(match kind {
            "Character" if id < FIRST_PLAYER_ID => format!("{base}/characters/{id}/portrait?size={size}"),
            "Corporation" if id < FIRST_PLAYER_ID => format!("{base}/corporations/{id}/logo?size={size}"),
            "Alliance" if id < FIRST_PLAYER_ID => format!("{base}/alliances/{id}/logo?size={size}"),
            "Type" => format!("{base}/types/{id}/icon?size={}", size.min(MAX_TYPE_ICON_SIZE)),
            "Render" => format!("{base}/types/{id}/render?size={size}"),
            _ => return None,
        })
    }

    async fn fetch(&self, kind: &str, id: u64, size: u32) -> Option<Vec<u8>> {
        let url = self.url(kind, id, size)?;
        let (_, client) = self.0.as_deref()?;
        let response = client.get(&url).send().await.ok()?;
        if !response.status().is_success() {
            warn!("images: upstream {url} -> {}", response.status());
            return None;
        }
        let body = response.bytes().await.ok()?;
        info!("images: fetched {url} ({} bytes)", body.len());
        Some(body.to_vec())
    }
}

/// Serve on `bind`; `kind` selects the request-log label ("images", "cdn") and
/// this listener's fixed admission bounds.
pub(crate) async fn serve(kind: ListenerKind, bind: String, root: PathBuf, upstream: Upstream, upload_limit_bytes: usize) -> Result<()> {
    for k in KINDS {
        tokio::fs::create_dir_all(root.join(k)).await?;
    }
    let listener = TcpListener::bind(&bind).await?;
    let (name, maximum, per_address) = match kind {
        ListenerKind::Images => ("images", IMAGE_MAX_CONNECTIONS, IMAGE_MAX_CONNECTIONS_PER_ADDRESS),
        ListenerKind::Cdn => ("cdn", CDN_MAX_CONNECTIONS, CDN_MAX_CONNECTIONS_PER_ADDRESS),
    };
    let gate = crate::admission::Admission::new(maximum, per_address);
    info!("{name}: listening on {bind}, storing images in {}", root.display());
    loop {
        let (stream, peer) = listener.accept().await?;
        let Ok(read_deadline) = crate::admission::deadline(Instant::now(), HTTP_READ_TIMEOUT_SECS) else { continue };
        let Some(permit) = gate.try_acquire(peer.ip()) else { continue };
        let (root, upstream) = (root.clone(), upstream.clone());
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle(stream, &root, name, peer, &upstream, upload_limit_bytes, read_deadline).await {
                warn!("{name}: request from {peer} failed: {e:#}");
            }
        });
    }
}

/// Read one LF-terminated line of at most `limit` bytes, terminator included,
/// appending to `out`. Rejects an overlong line and EOF without a terminator.
async fn bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R, out: &mut Vec<u8>, limit: usize) -> Result<()> {
    let bound = u64::try_from(limit).ok().and_then(|n| n.checked_add(1)).context("line limit overflows")?;
    let start = out.len();
    reader.take(bound).read_until(b'\n', out).await?;
    let line = &out[start..];
    anyhow::ensure!(line.last() == Some(&b'\n') && line.len() <= limit, "line exceeds {limit} bytes or lacks a terminator");
    Ok(())
}

/// One request, for the log line written with its response.
struct Req<'a> {
    name: &'static str,
    peer: std::net::SocketAddr,
    method: &'a str,
    path: &'a str,
}

/// `/Character/90000001_512.jpg` -> `<root>/Character/90000001_512.jpg`, rejecting anything else.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let path = path.split('?').next()?.trim_start_matches('/');
    let (kind, file) = path.split_once('/')?;
    if !KINDS.contains(&kind) {
        return None;
    }
    let (stem, ext) = file.rsplit_once('.')?;
    let (id, size) = stem.split_once('_')?;
    if !matches!(ext, "jpg" | "png") || id.parse::<u64>().is_err() || size.parse::<u32>().is_err() {
        return None;
    }
    Some(root.join(kind).join(file))
}

async fn respond(w: &mut TcpStream, req: &Req<'_>, status: &str, ctype: &str, body: &[u8], head_only: bool) -> Result<()> {
    if status.starts_with('2') {
        info!("{}: {} {} {} -> {status} ({} bytes)", req.name, req.peer, req.method, req.path, body.len());
    } else {
        warn!("{}: {} {} {} -> {status}", req.name, req.peer, req.method, req.path);
    }
    let hdr = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    w.write_all(hdr.as_bytes()).await?;
    if !head_only {
        w.write_all(body).await?;
    }
    Ok(())
}

/// The image at `file`, else the nearest stored size of it (larger first), else fetched
/// from upstream and stored.
async fn find(file: &Path, upstream: &Upstream) -> Option<Vec<u8>> {
    if let Ok(data) = tokio::fs::read(file).await {
        return Some(data);
    }
    let kind = file.parent()?.file_name()?.to_str()?;
    let stem = file.file_stem()?.to_str()?;
    let ext = file.extension()?.to_str()?;
    let (id, size) = stem.split_once('_')?;
    let (id, size): (u64, u32) = (id.parse().ok()?, size.parse().ok()?);
    let mut sizes: Vec<u32> = SIZES.to_vec();
    sizes.sort_by_key(|s| (*s < size, s.abs_diff(size)));
    for s in sizes {
        for e in [ext, if ext == "jpg" { "png" } else { "jpg" }] {
            if let Ok(data) = tokio::fs::read(file.with_file_name(format!("{id}_{s}.{e}"))).await {
                return Some(data);
            }
        }
    }
    let data = upstream.fetch(kind, id, size).await?;
    if let Err(e) = tokio::fs::write(file, &data).await {
        warn!("images: can't store {}: {e}", file.display());
    }
    Some(data)
}

async fn handle(mut stream: TcpStream, root: &Path, name: &'static str, peer: std::net::SocketAddr, upstream: &Upstream, upload_limit_bytes: usize, read_deadline: Instant) -> Result<()> {
    let mut reader = BufReader::new(&mut stream);
    let deadline = tokio::time::Instant::from_std(read_deadline);
    // Read raw bytes: a request line or header may not be UTF-8.
    let mut raw = Vec::new();
    tokio::time::timeout_at(deadline, bounded_line(&mut reader, &mut raw, HTTP_REQUEST_LINE_LIMIT_BYTES)).await??;
    let line = String::from_utf8_lossy(&raw).into_owned();
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let mut content_length = 0usize;
    let mut headers = Vec::new();
    loop {
        // Every header line and the closing blank line share the total budget.
        let before = headers.len();
        let remaining = HTTP_HEADER_LIMIT_BYTES.checked_sub(before).context("headers exceed the total budget")?;
        tokio::time::timeout_at(deadline, bounded_line(&mut reader, &mut headers, remaining)).await??;
        let line = &headers[before..];
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        let h = String::from_utf8_lossy(line);
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let upload = matches!(method.as_str(), "PUT" | "POST");
    let mut body = Vec::new();
    if upload && content_length > 0 && content_length <= upload_limit_bytes {
        body = vec![0u8; content_length];
        tokio::time::timeout_at(deadline, reader.read_exact(&mut body)).await??;
    }
    drop(reader);
    let req = Req { name, peer, method: &method, path: &path };
    if upload && (content_length == 0 || content_length > upload_limit_bytes) {
        warn!("{name}: {peer} {method} {path}: body of {content_length} bytes rejected (limit {upload_limit_bytes})");
    }
    if let Some(name) = path.strip_prefix("/content/") {
        return match infinity_content::cdn::content(name.split('?').next().unwrap_or("")) {
            Some((data, _)) if matches!(method.as_str(), "GET" | "HEAD") => {
                respond(&mut stream, &req, "200 OK", "application/octet-stream", data, method == "HEAD").await
            }
            _ => respond(&mut stream, &req, "404 Not Found", "text/plain", b"not found", method == "HEAD").await,
        };
    }
    let Some(file) = resolve(root, &path) else {
        return respond(&mut stream, &req, "404 Not Found", "text/plain", b"not found", method == "HEAD").await;
    };
    match method.as_str() {
        "GET" | "HEAD" => match find(&file, upstream).await {
            Some(data) => {
                // By content: a fallback may be the other format.
                let ctype = if data.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
                respond(&mut stream, &req, "200 OK", ctype, &data, method == "HEAD").await
            }
            None => respond(&mut stream, &req, "404 Not Found", "text/plain", b"not found", method == "HEAD").await,
        },
        "PUT" | "POST" => {
            if body.is_empty() {
                return respond(&mut stream, &req, "413 Payload Too Large", "text/plain", b"bad size", false).await;
            }
            tokio::fs::write(&file, &body).await?;
            info!("{name}: stored {} ({} bytes) from {peer}", file.display(), body.len());
            respond(&mut stream, &req, "201 Created", "text/plain", b"stored", false).await
        }
        _ => {
            respond(&mut stream, &req, "405 Method Not Allowed", "text/plain", b"method not allowed", false).await
        }
    }
}

#[cfg(all(test, any(feature = "tests-runtime", feature = "tests-sde")))]
mod tests;
