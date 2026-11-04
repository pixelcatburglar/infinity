//! Space checkpoint for `provider.py:MapTagProvider`.
//! Uses server CDN content and descriptors, not World; it does not implement
//! live map state.
use anyhow::Result;
use prost_reflect::DynamicMessage;
use super::{context::HandlerContext, content::checkpoint};

pub(super) fn handle(ctx: &HandlerContext<'_>, name: &str) -> Result<DynamicMessage> {
    checkpoint(ctx, name, "maptags")
}
