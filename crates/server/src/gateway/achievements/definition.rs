//! Achievement checkpoint for `content_provider.py:AchievementDefinitionsProvider`.
//! Uses server CDN content and descriptors, not World; it does not implement
//! achievement progress or event processing.
use anyhow::Result;
use prost_reflect::DynamicMessage;
use super::super::{Gateway, content::checkpoint};

pub(in crate::gateway) fn handle(gw: &Gateway, name: &str) -> Result<DynamicMessage> {
    checkpoint(gw, name, "achievements")
}
