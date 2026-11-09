//! Live corporation authority for Hub mutations, using effective roles and titles.
//! Callers hold canonical organization admission when this is used for a write.
//! Session corporation and client-supplied owner IDs do not grant authority.
use infinity_core::{consts::ROLE_DIRECTOR, messages};
use crate::{db::executor::Executor, services::{corps::permissions, notify_error, CallError}};

use std::collections::{BTreeMap, BTreeSet};
use crate::dao::{alliances, characters::roles, sovereignty::sources::{self, SourceChange}};
use super::CaptureContribution;

pub(crate) struct ContributorValidation {
    pub(crate) valid: bool,
    pub(crate) changes: Vec<SourceChange>,
}

/// Validate frozen proofs while the caller holds canonical organization admission.
pub(crate) async fn validate_contributors(
    ex: &(impl Executor + ?Sized), contributions: &[CaptureContribution], observation_time: i64,
) -> Result<ContributorValidation, CallError> {
    let characters: Vec<_> = contributions.iter().map(|p| p.contributor.character_id).collect::<BTreeSet<_>>().into_iter().collect();
    let corps: Vec<_> = contributions.iter().map(|p| p.contributor.corporation_id).collect::<BTreeSet<_>>().into_iter().collect();
    let authorities: BTreeMap<_, _> = roles::authorities(ex, &characters).await.map_err(crate::services::db_err)?
        .into_iter().map(|a| (a.character, a)).collect();
    let versions: BTreeMap<_, _> = sources::versions(ex, &characters).await.map_err(crate::services::db_err)?
        .into_iter().map(|v| (v.character_id, v.revision)).collect();
    let corp_alliances: BTreeMap<_, _> = alliances::live_corporation_alliances(ex, &corps).await.map_err(crate::services::db_err)?
        .into_iter().map(|row| (row.corporation_id, row.alliance_id)).collect();
    let mut valid = true;
    let mut historical = BTreeSet::new();
    for proof in contributions {
        let c = &proof.contributor;
        valid &= proof.through <= observation_time && c.source_revision >= 0;
        match versions.get(&c.character_id) {
            Some(&revision) if revision == c.source_revision => {
                valid &= authorities.get(&c.character_id).is_some_and(|a| a.live && a.corporation == c.corporation_id)
                    && corp_alliances.get(&c.corporation_id) == Some(&Some(c.alliance_id));
            }
            Some(&revision) if revision > c.source_revision => { historical.insert((c.character_id, c.source_revision)); }
            _ => valid = false,
        }
    }
    let mut changes = Vec::new();
    for (character, revision) in historical {
        match sources::first_change_after(ex, character, revision).await.map_err(crate::services::db_err)? {
            Some(change) => {
                valid &= contributions.iter().filter(|p| p.contributor.character_id == character && p.contributor.source_revision == revision)
                    .all(|p| p.through <= change.effective_at);
                changes.push(change);
            }
            None => valid = false,
        }
    }
    Ok(ContributorValidation { valid, changes })
}

pub(crate) async fn director(
    ex: &(impl Executor + ?Sized), character: i64, corporation: i64,
) -> Result<permissions::Authority, CallError> {
    let refused = || notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION);
    let authority = permissions::load(ex, character).await?.ok_or_else(refused)?;
    if !permissions::allows(&authority, corporation, ROLE_DIRECTOR) { return Err(refused()); }
    Ok(authority)
}
