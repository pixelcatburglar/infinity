//! Title checkpoint for `provider.py:parse_response`.
//! Uses server CDN content and descriptors, not World; this does not implement
//! title ownership or assignment.
use anyhow::Result;
use prost_reflect::DynamicMessage;
use super::super::{context::HandlerContext, content::checkpoint};

pub(in crate::gateway) fn handle(ctx: &HandlerContext<'_>, name: &str) -> Result<DynamicMessage> {
    checkpoint(ctx, name, "titles")
}
