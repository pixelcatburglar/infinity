//! GM online operations for fitted modules, using the shared online policy.
//! `eve/client/script/environment/godma.py` consumes the resulting effect changes.
//! Reads game items and dogma attributes and SDE type and effect data.
//! Does not online items outside the fitted modules accepted by the online policy.
use crate::client::Client;
use crate::services::{db_err, CallError};
use crate::services::identity::Identity;
use crate::services::items::load_items;
use crate::dao::items::loading::ItemQuery;
use infinity_core::consts::dogma::EFFECT_ONLINE;

/// Set fitted modules online and notify the controlling client after commit.
pub(in crate::services) async fn gm_online(c: &mut Client, ship: i64) -> Result<usize, CallError> {
    use crate::services::effects::online::{dispatch_changed, set_online_core, OnlineChanged, OnlinePolicy};
    use crate::db::outbox::Outbox;
    let _lock = crate::services::market::lock().await;
    let policy = OnlinePolicy::from_identity(&Identity::of_client(c), c.notify_tx.clone())?;
    let fences = crate::services::effects::online::capture_online_fences(&c.world, &[policy.owner]).await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let policy = policy.clone();
            let fences = fences.clone();
            Box::pin(async move {
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? {
                    return Err(crate::services::notify_error(infinity_core::messages::gm::GM_ITEM_NOT_FOUND));
                }
                load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next()
                    .filter(|item| item.owner_id == policy.owner && item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP))
                    .ok_or_else(|| crate::services::notify_error(infinity_core::messages::gm::GM_ITEM_NOT_FOUND))?;
                let mut fitted = load_items(tx, &sde, ItemQuery::LocationOwner(ship, policy.owner)).await.map_err(db_err)?;
                fitted.sort_unstable_by_key(|item| item.item_id);
                let mut count = 0;
                let mut before = None;
                let mut final_fit = None;
                for item in fitted.iter().filter(|item| item.category_id == i64::from(infinity_core::consts::CATEGORY_MODULE)
                    && item.singleton && crate::services::inventory::is_fitting_flag(item.flag_id)
                    && crate::services::effects::lifecycle::has_effect(sde.types.effects(item.type_id), EFFECT_ONLINE as i32)) {
                    if before.is_none() { before = Some(crate::services::effects::online::fitting::capture(tx, &sde, &fences, policy.owner, ship).await?); }
                    let (changed, fit) = set_online_core(tx, &sde, &policy, outbox, &fences, ship, item.item_id, true).await?;
                    count += usize::from(changed);
                    final_fit = Some(fit);
                }
                if let (Some(before), Some(final_fit)) = (before, final_fit) {
                    crate::services::effects::online::fitting::finish(tx, before, final_fit, outbox).await?;
                }
                Ok(count)
            })
        },
        |count, outbox| async move { dispatch_changed(count, outbox, c) },
    ).await
}
