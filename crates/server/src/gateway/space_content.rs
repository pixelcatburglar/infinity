//! Space checkpoint for `provider.py:MapTagProvider`.
//! Uses server CDN content and descriptors, not World; it does not implement
//! live map state.
use anyhow::Result;
use prost_reflect::DynamicMessage;
use super::{Gateway, content::checkpoint};

pub(super) fn handle(gw: &Gateway, name: &str) -> Result<DynamicMessage> {
    checkpoint(gw, name, "maptags")
}
