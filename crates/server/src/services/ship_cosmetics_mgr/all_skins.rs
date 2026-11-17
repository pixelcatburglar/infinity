//! Bulk permanent first-party SKIN grants for the `/allskins` GM command.
//! There is no client service mirror: `cosmeticsSvc.py` consumes the same
//! `OnSkinLicenseActivated` notifications and license queries as item activation.
//! Licenses persist in `game.ship_skin_licenses`, applications in
//! `game.ship_skin_applications` and deadlines in `game.jobs`; eligibility is the
//! complete region-visible SDE skin set, not the license-item subset.
//! Structure, single-use and SKINR entitlements, ship application and
//! revocation are not granted here.

use std::sync::Arc;
use marshal::Value;
use infinity_core::messages::ship_skins as text;
use crate::{client::filetime_now, dao::{characters::{lookup, selection::lock_character}, ship_skins}, db::{executor::Executor, outbox::{dispatch_world, Outbox}, Db}, sde::Sde, services::{db_err, market, notify_error, CallError}, space::World};
use super::expiry;

/// Outcome of one bulk grant: `changed` counts new grants, timed upgrades and
/// re-grants after expiry cleanup, `already_permanent` the skipped rows.
pub(crate) struct Summary {
    pub changed: i64,
    pub already_permanent: i64,
}

/// Grant every ordinary region-visible SKIN permanently in one transaction.
/// The target need not be online; the market guard is held until post-commit
/// dispatch, and an unchanged run sends nothing and wakes no scheduler.
pub(crate) async fn grant(db: &Db, world: &World, sde: &Arc<Sde>, region: &str, character: i64) -> Result<Summary, CallError> {
    let _guard = market::lock().await;
    let targets = expiry::prepare(db, world, character).await?;
    let now = filetime_now();
    let sde = sde.clone();
    let region = region.to_owned();
    db.transaction(move |tx, outbox: &mut Outbox| {
        let sde = sde.clone(); let region = region.clone(); let targets = targets.clone();
        Box::pin(async move { grant_tx(tx, &sde, &region, character, now, &targets, outbox).await })
    }, |(summary, changed), outbox| async move {
        let result = dispatch_world(summary, outbox, world);
        if changed { crate::scheduler::wake(); }
        result
    }).await
}

/// The complete bulk-grant transaction: lock the target and reconfirm it is an
/// active character, remove due licenses, then run the whole grant set through
/// one conditional upsert that returns only the changed skin IDs. Sorted
/// returned rows drive the activation notifications, and the nearest deadline
/// is rescheduled only when something changed; the flag drives one wake after
/// commit.
async fn grant_tx(ex: &(impl Executor + ?Sized), sde: &Sde, region: &str, character: i64, now: i64, targets: &expiry::Targets, outbox: &mut Outbox) -> Result<(Summary, bool), CallError> {
    lock_character(ex, character).await.map_err(db_err)?;
    if lookup::find(ex, "", Some(character)).await.map_err(db_err)?.is_none() { return Err(notify_error(text::INVALID_ARGUMENT)); }
    let expiry_changed = expiry::expire_due_tx(ex, character, now, targets, outbox).await?;
    let candidates: Vec<i32> = sde.ship_skins.ordinary_skins(region).map(|skin| skin.skin_id).collect();
    let mut granted = ship_skins::grant_permanent_licenses(ex, character, &candidates).await.map_err(db_err)?;
    granted.sort_unstable_by_key(|row| row.skin_id);
    let summary = Summary { changed: granted.len() as i64, already_permanent: candidates.len() as i64 - granted.len() as i64 };
    for row in &granted {
        outbox.notify_pilot(character, "OnSkinLicenseActivated", vec![Value::Int(i64::from(row.skin_id)), Value::Int(character)]);
    }
    expiry::commit_changes_tx(ex, sde, character, now, targets, outbox).await?;
    let changed = expiry_changed || summary.changed != 0;
    if changed { expiry::schedule(ex, character).await?; }
    Ok((summary, changed))
}

#[cfg(all(test, feature = "tests-ships"))]
mod tests;
