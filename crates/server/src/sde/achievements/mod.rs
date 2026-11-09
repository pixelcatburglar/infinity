//! Immutable verified achievement definitions from the retail ACHV content.
//! Mirrors `generated.py:AchievementData`; the original
//! content remains the sole CDN source. Progress, rewards and event handling are
//! not implemented here.
//!
//! Generated with flatc 25.2.10:
//! `flatc --rust -o crates/server/src/sde/achievements crates/server/src/sde/achievements/achievement.fbs`

#[allow(dead_code, unused_imports, mismatched_lifetime_syntaxes)]
pub(crate) mod achievement_generated;
mod validate;

use anyhow::{ensure, Context, Result};
use achievement_generated as format;

/// A verified immutable view of the content and its precomputed checkpoint.
pub(crate) struct Achievements {
    root: format::AchievementData<'static>,
    metadata: CheckpointMetadata,
}

pub(crate) struct CheckpointMetadata {
    pub(crate) version: (u32, u32, u32),
    pub(crate) crc: u32,
}

impl Achievements {
    pub(crate) fn load(file: crate::retail::CdnFile) -> Result<Self> {
        ensure!(file.data.len() >= 8 && format::achievement_data_buffer_has_identifier(file.data), "invalid achievement identifier");
        let root = format::root_as_achievement_data(file.data).context("invalid achievement FlatBuffer")?;
        let catalogue = Self { root, metadata: CheckpointMetadata { version: file.version, crc: crate::cdn::crc32(file.data) } };
        validate::validate(catalogue.catalogue()).context("invalid achievement catalogue")?;
        Ok(catalogue)
    }
    /// Borrow the complete verified tree in its original vector order.
    pub(crate) fn catalogue(&self) -> format::AchievementData<'static> {
        self.root
    }


    pub(crate) fn checkpoint_metadata(&self) -> &CheckpointMetadata {
        &self.metadata
    }
}

#[cfg(test)] mod tests;
