//! Atomic settlement of frozen daily rewards through the shared economy authorities.
//! The daily-goal client redeems completed rights into character balances and hangars.
//! Receipts and effects use the daily-goal tables and the account PLEX vault.
//! Expired completed rights remain redeemable; redemption tokens are not used.
use std::sync::Arc;
use infinity_core::{
    consts::{CORP_HERALDRY, FLAG_HANGAR, IX_LOCATION_ID},
};
use crate::{
    dao::{accounts::subscriptions, characters::{attributes, clones, ship, social}, daily_goals::{instances, rewards}, items::transfers},
    db::outbox::{self, Outbox},
    lua::{content::daily_goals::plans::{self, ProjectInput, Projection}, events::resources::{FactContext, FactOrigin, LoyaltyReason}},
    services::{identity::Identity, inventory::notifications::{notify_item_change, route_target}, market, plex_vault::{self, VaultAudience, VaultChange, VaultContext, VaultKind}, standings::{self, LoyaltyMutation}, subscriptions::rules, wallets},
};
use marshal::Value;
use super::{facts, queries, wire, Error, Resources};

enum Outcome { Done, Stale }

/// Settle all currently eligible unclaimed assets in a single transaction.
pub(crate) async fn redeem(resources: &Resources, identity: &Identity, user: i64, id: &str, current_location: bool, at: i64) -> Result<(), Error> {
    let character = identity.char_id;
    let id = id.to_owned();
    loop {
        let prepared = queries::read_one(resources, user, character, &id, at).await?;
        let keys = plans::project(&prepared.policy, &ProjectInput { entries: vec![Projection { key: id.clone(), selected: true }] }, None)?;
        if !keys.keys.iter().any(|key| key == &id) { return Err(Error::Missing); }
        let grade_revision = subscriptions::by_user(&resources.db, user).await?.revision;
        let location = if prepared.view.rewards.iter().any(|r| r.unit == "item" && r.claimed_at.is_none()) {
            let current = ship::current(&resources.db, character).await?.ok_or(Error::Forbidden)?.station;
            let home = social::home_station_id(&resources.db, character).await?;
            Some((current, home))
        } else { None };
        let notice = wire::redeemed_notice(character, &id)?;
        let target = u32::try_from(character).map_err(|_| Error::Invalid)?;
        let prepared = Arc::new(prepared);
        let sde = Arc::clone(&resources.sde);
        let _market = market::lock().await;
        let result = resources.db.transaction::<_, _, Error, Outbox, _, _, _>(|tx, outbox| {
            let notice = notice.retarget(crate::gateway::protocol::target_group::Target::Character(target)).map_err(|e| Error::from(crate::gateway::notices::producer_error(e)));
            let prepared = Arc::clone(&prepared);
            let id = id.clone();
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let id = id.as_str();
                let goal = instances::lock_sorted(tx, character, &[id.to_owned()]).await?.pop().ok_or(Error::Missing)?;
                let notice = notice?;
                let locked_grade = subscriptions::lock(tx, user).await?;
                let locked_location = if location.is_some() {
                    let state = clones::lock_character(tx, character).await?.ok_or(Error::Forbidden)?;
                    Some((state.station_id, social::home_station_id(tx, character).await?))
                } else { None };
                let mut assets = rewards::list(tx, character, id, true).await?;
                if goal != prepared.view.goal || assets != prepared.view.rewards || locked_grade.revision != grade_revision || locked_location != location { return Ok(Outcome::Stale); }
                if !assets.is_empty() && assets.iter().all(|r| r.claimed_at.is_some()) { outbox.notify_gateway(notice); return Ok(Outcome::Done); }
                if goal.completed_at.is_none() || goal.progress != goal.target || goal.voided_at.is_some() { return Err(Error::Forbidden); }
                let omega = rules::effective_grade(&locked_grade, at) == rules::OMEGA;
                let eligible = |reward: &rewards::RewardRow| reward.claimed_at.is_none() && (!reward.omega_required || goal.earned_omega || omega);
                if !assets.iter().any(eligible) {
                    if assets.iter().any(|r| r.claimed_at.is_some()) { outbox.notify_gateway(notice); return Ok(Outcome::Done); }
                    return Err(Error::Forbidden);
                }
                let destination = if assets.iter().any(|r| r.unit == "item" && eligible(r)) {
                    let (current, home) = locked_location.ok_or(Error::Forbidden)?;
                    let station = if current_location { current } else { home };
                    Some(station.filter(|station| sde.map.station(*station).is_some()).ok_or(Error::Forbidden)?)
                } else { None };
                for asset in &mut assets {
                    if !eligible(asset) { continue; }
                    if !asset.earned {
                        if !rewards::earn(tx, character, id, asset.reward_index, asset.revision).await? { return Err(Error::Unavailable); }
                        asset.earned = true;
                        asset.revision += 1;
                    }
                    let mut item = None;
                    let mut plex_reference = None;
                    match asset.unit.as_str() {
                        "isk" => wallets::credit(tx, outbox, character, asset.amount_isk.ok_or(Error::Invalid)?).await?,
                        "lp" => {
                            let amount = i64::try_from(asset.amount_units.ok_or(Error::Invalid)?).map_err(|_| Error::Invalid)?;
                            standings::add_lp(tx, outbox, character, asset.issuer_id.ok_or(Error::Invalid)?, amount, LoyaltyMutation { reason: LoyaltyReason::DailyReward, occurred_at: at, context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Natural } }).await?;
                        }
                        "sp" => {
                            let amount = i64::try_from(asset.amount_units.ok_or(Error::Invalid)?).map_err(|_| Error::Invalid)?;
                            let balance = attributes::grant_free_points(tx, character, amount).await?.ok_or(Error::Unavailable)?;
                            outbox.notify_pilot(character, "OnFreeSkillPointsChanged", vec![Value::Int(balance)]);
                        }
                        "item" => {
                            let station = destination.ok_or(Error::Forbidden)?;
                            let amount = i32::try_from(asset.amount_units.ok_or(Error::Invalid)?).map_err(|_| Error::Invalid)?;
                            let item_id = transfers::insert(tx, asset.type_id.ok_or(Error::Invalid)?, character, station, FLAG_HANGAR, i64::from(amount)).await?;
                            let mut child = Outbox::default();
                            notify_item_change(tx, &sde, &mut child, item_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                            outbox.append_routed(child, |n| route_target(n, character, None));
                            item = Some(item_id);
                        }
                        "plex" => {
                            let amount = i128::from(asset.amount_units.ok_or(Error::Invalid)?);
                            let reference = crate::dao::accounts::plex::reference(tx).await?;
                            plex_vault::change_in(tx, outbox, user, Some(character), VaultChange::Add(amount), VaultContext { kind: VaultKind::DailyReward, item_id: None, reference: Some(reference) }, VaultAudience::Account).await?.ok_or(Error::Unavailable)?;
                            plex_reference = Some(reference);
                        }
                        _ => return Err(Error::Invalid),
                    }
                    let reward_location = if asset.unit == "item" { destination } else { None };
                    if !rewards::claim(tx, character, id, asset.reward_index, asset.revision, at, reward_location, item, plex_reference).await? { return Err(Error::Unavailable); }
                    asset.claimed_at = Some(at);
                    asset.location_id = reward_location;
                    asset.item_id = item;
                    asset.plex_reference = plex_reference;
                    asset.revision += 1;
                    if asset.unit == "lp" && asset.issuer_id == Some(CORP_HERALDRY) { facts::evermarks(outbox, &goal, asset, at)?; }
                }
                let has_unclaimed = assets.iter().any(|r| r.claimed_at.is_none());
                if !instances::set_unclaimed(tx, character, id, goal.revision, has_unclaimed).await? { return Err(Error::Unavailable); }
                outbox.notify_gateway(notice);
                Ok(Outcome::Done)
            })
        }, |value, outbox| async move { outbox::dispatch_world(value, outbox, &resources.world).map_err(Error::from) }).await?;
        match result { Outcome::Done => return Ok(()), Outcome::Stale => continue }
    }
}

#[cfg(test)] mod tests;
