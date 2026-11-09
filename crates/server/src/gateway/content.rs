//! CDN checkpoints consumed by `provider.py:parse_response`.
//! Uses server CDN content and embedded descriptors, not World or DAO data;
//! only registered content keys are available.
use anyhow::{Context, Result};
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::cdn;
use super::{context::HandlerContext, message::{response_for, child, set}};

/// `{checkpoint: {url, version, crc}}` pointing at crate::cdn content.
pub(super) fn checkpoint(ctx: &HandlerContext<'_>, request: &str, content: &str) -> Result<DynamicMessage> {
    let (data, version) = cdn::content(content).context("unknown content")?;
    tracing::debug!("gateway: checkpoint {content} ({} bytes) for {request}", data.len());
    checkpoint_from_metadata(ctx, request, content, version, cdn::crc32(data))
}

pub(super) fn checkpoint_from_metadata(ctx: &HandlerContext<'_>, request: &str, content: &str, (major, minor, patch): (u32, u32, u32), checksum: u32) -> Result<DynamicMessage> {
    let mut rsp = response_for(request)?;
    let mut cp = child(&rsp.descriptor(), "checkpoint")?;
    set(&mut cp, "url", PV::String(format!("{}/content/{content}", ctx.config.cdn_url())))?;
    let mut version = child(&cp.descriptor(), "version")?;
    set(&mut version, "major", PV::U32(major))?;
    set(&mut version, "minor", PV::U32(minor))?;
    set(&mut version, "patch", PV::U32(patch))?;
    set(&mut cp, "version", PV::Message(version))?;
    let mut crc = child(&cp.descriptor(), "crc")?;
    set(&mut crc, "crc", PV::U32(checksum))?;
    set(&mut cp, "crc", PV::Message(crc))?;
    set(&mut rsp, "checkpoint", PV::Message(cp))?;
    Ok(rsp)
}

#[cfg(test)] mod tests;
