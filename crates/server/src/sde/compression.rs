//! Immutable compression mappings from `sde_native.compressibletypes` loaded at startup.
//! `itemcompression/client/ui/compression_window.py:150-157` previews one output unit per source unit.
//! Facility eligibility and conversion execution are handled separately.
//! An empty or invalid mapping fails startup.
use std::collections::HashMap;
use crate::{dao::sde::compression as dao, db::Db};
use super::types::Types;

#[cfg_attr(test, derive(Default))]
pub(crate) struct Compression(HashMap<i32, i32>);

impl Compression {
    pub(crate) async fn load(db: &Db, types: &Types) -> anyhow::Result<Self> {
        Self::from_rows(dao::load(db).await?, |id| types.get(id).is_some())
    }

    fn from_rows(rows: Vec<dao::CompressionRow>, exists: impl Fn(i32) -> bool) -> anyhow::Result<Self> {
        let mut mappings = HashMap::new();
        for row in rows {
            let source: i32 = row.source.as_deref().ok_or_else(|| anyhow::anyhow!("missing compression source"))?.parse()?;
            let output: i32 = row.output.as_deref().ok_or_else(|| anyhow::anyhow!("missing compression output"))?.parse()?;
            anyhow::ensure!(source > 0 && output > 0 && source != output && exists(source) && exists(output), "invalid compression mapping {source} -> {output}");
            if let Some(previous) = mappings.insert(source, output) {
                anyhow::ensure!(previous == output, "conflicting compression mapping for {source}");
            }
        }
        anyhow::ensure!(!mappings.is_empty(), "compression mapping is empty");
        Ok(Self(mappings))
    }

    pub(crate) fn get(&self, source_type: i32) -> Option<i32> { self.0.get(&source_type).copied() }
}

#[cfg(test)] mod tests;
