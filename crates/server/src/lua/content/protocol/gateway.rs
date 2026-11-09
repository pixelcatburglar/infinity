//! Exact daily gateway descriptor pairs and bounded protobuf text conversion.
//! Uses the embedded public descriptor pool; arbitrary Lua-selected types are denied.

use anyhow::{ensure, Context, Result};
use prost_reflect::{DynamicMessage, MessageDescriptor, ReflectMessage};
use crate::gateway::{daily_goals, message::pool};

/// Server safety bound for a single gateway Lua protobuf text value.
const MAX_TEXT_BYTES: usize = 1024 * 1024;

pub(crate) fn descriptors(name: &str) -> Result<(MessageDescriptor, MessageDescriptor)> {
    ensure!(daily_goals::supported(name), "unsupported gateway request {name}");
    let request = pool().get_message_by_name(name).with_context(|| format!("missing request descriptor {name}"))?;
    let response_name = format!("{}Response", name.strip_suffix("Request").context("invalid request name")?);
    let response = pool().get_message_by_name(&response_name).with_context(|| format!("missing response descriptor {response_name}"))?;
    Ok((request, response))
}

pub(crate) fn encode(message: &DynamicMessage, expected: &MessageDescriptor) -> Result<String> {
    ensure!(message.descriptor() == *expected, "unexpected gateway message descriptor");
    let text = message.to_text_format();
    ensure!(text.len() <= MAX_TEXT_BYTES, "gateway message text exceeds limit");
    Ok(text)
}


#[cfg(test)] mod tests;
