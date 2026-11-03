//! Title checkpoint for `provider.py:parse_response`.
//! Uses server CDN content and descriptors, not World; this does not implement
//! title ownership or assignment.
use anyhow::Result;
use prost_reflect::DynamicMessage;
use super::super::{Gateway, content::checkpoint};

pub(in crate::gateway) fn handle(gw: &Gateway, name: &str) -> Result<DynamicMessage> {
    checkpoint(gw, name, "titles")
}
