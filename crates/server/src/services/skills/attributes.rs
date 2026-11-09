//! Character training attributes, mirroring characterskills/util.py.
//! Base values use persisted remaps or startup SDE bloodlines as fallback.
//! Effective values use Fit, including active character implants and boosters.
use super::{db_err, CallError};
use crate::client::Client;
use infinity_core::consts::dogma::CHAR_ATTRIBUTES;
use crate::db::executor::Executor;
use crate::services::fit::{Fit, Key};
use std::collections::HashMap;
use std::sync::Arc;

/// Server fallback for missing SDE bloodline attributes.
const FALLBACK_BLOODLINE_ATTRIBUTE: i32 = 5;
/// Server character attribute calculation offset.
const CHARACTER_ATTRIBUTE_OFFSET: f64 = 17.0;

/// Effective character attribute values {attributeID: value}.
pub(crate) async fn character_attributes(c: &Client, char_id: i64) -> Result<HashMap<i64, f64>, CallError> {
    character_attributes_db(&c.db, &c.sde, char_id).await
}

pub(super) async fn character_attributes_db(db: &(impl Executor + ?Sized), sde: &Arc<crate::sde::Sde>, char_id: i64) -> Result<HashMap<i64, f64>, CallError> {
    let fit = Fit::load(db, Arc::clone(sde), char_id, 0).await?;
    Ok(CHAR_ATTRIBUTES.iter().map(|&attr| (attr, fit.value(Key::Char, attr as i32))).collect())
}

pub(super) async fn character_attributes_at(db: &(impl Executor + ?Sized), sde: &Arc<crate::sde::Sde>, char_id: i64, at: i64) -> Result<HashMap<i64, f64>, CallError> {
    let fit = Fit::load_character_at(db, Arc::clone(sde), char_id, at).await?;
    Ok(CHAR_ATTRIBUTES.iter().map(|&attr| (attr, fit.value(Key::Char, attr as i32))).collect())
}

/// Persisted remap or bloodline-derived starting values before modifiers.
pub(crate) async fn base_character_attributes(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, char_id: i64) -> Result<HashMap<i64, f64>, CallError> {
    if let Some(bases) = crate::dao::characters::attributes::respec(db, char_id).await.map_err(db_err)?.and_then(|r| r.bases) {
        return Ok(CHAR_ATTRIBUTES.iter().zip(bases).map(|(a, value)| (*a, value as f64)).collect());
    }
    let id = crate::dao::characters::attributes::bloodline(db, char_id).await.map_err(db_err)?;
    let r = sde.characters.bloodline(id as i64).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing bloodline {id} for character {char_id}")))?;
    let values = [r.charisma, r.intelligence, r.memory, r.perception, r.willpower];
    Ok(CHAR_ATTRIBUTES.iter().zip(values).map(|(a, value)| (*a, value.unwrap_or(FALLBACK_BLOODLINE_ATTRIBUTE) as f64 + CHARACTER_ATTRIBUTE_OFFSET)).collect())
}
