//! Achievement checkpoint for `content_provider.py:AchievementDefinitionsProvider`.
//! Uses server CDN content and descriptors, not World; it does not implement
//! achievement progress or event processing.
use anyhow::Result;
use prost_reflect::DynamicMessage;
use super::super::{context::HandlerContext, content::checkpoint};

pub(in crate::gateway) fn handle(ctx: &HandlerContext<'_>, name: &str) -> Result<DynamicMessage> {
    checkpoint(ctx, name, "achievements")
}
