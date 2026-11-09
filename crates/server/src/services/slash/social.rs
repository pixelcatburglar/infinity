//! GM standing and loyalty-point controls, using standings service notifications.
//! Standings and balances are persisted in the owning service's game tables.
//! A concurrent mission reward between /setlp's read and adjustment can change
//! the final balance; the adjustment itself is atomic and never makes it negative.
use super::{slash_error, Client, CallError};
use infinity_core::messages;
use crate::{db::outbox::dispatch_world, services::{corps, standings}};

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    let me = c.session.int("charid").ok_or_else(|| slash_error(messages::gm::GM_SOCIAL_USAGE.into()))?;
    let parsed = super::args::Args::new(args);
    match command {
        "corpcreate" | "setalliance" => {
            if args.len() != 2 { return Err(slash_error(messages::gm::GM_ORG_CREATE_USAGE.into())); }
            let id = if command == "corpcreate" {
                corps::corp_registry::gm_found(c, &args[0], &args[1]).await?
            } else {
                crate::services::corps::alliance_corp_registry::gm_create(c, &args[0], &args[1]).await?
            };
            Ok(messages::gm::gm_org_created(id))
        }
        "joinalliance" => {
            if args.len() != 1 { return Err(slash_error(messages::gm::GM_JOINALLIANCE_USAGE.into())); }
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_JOINALLIANCE_USAGE.into()))?;
            let alliance = parsed.owner(c, 0).await?;
            crate::services::corps::alliance_corp_registry::gm_join(c, alliance).await?;
            Ok(messages::gm::gm_joinalliance(alliance))
        }
        "missioncomplete" => {
            if args.len() > 1 { return Err(slash_error(messages::gm::GM_MISSIONCOMPLETE_USAGE.into())); }
            let agent = match args.first().map(String::as_str) {
                None | Some("all") => None,
                Some(_) => Some(parsed.i64(0).ok().filter(|id| *id > 0)
                    .ok_or_else(|| slash_error(messages::gm::GM_MISSIONCOMPLETE_USAGE.into()))?),
            };
            let count = crate::services::missions::force_complete(c, agent).await?;
            Ok(messages::gm::gm_missioncomplete(count))
        }
        "setstanding" => {
            if !(3..=4).contains(&args.len()) { return Err(slash_error(messages::gm::GM_SETSTANDING_USAGE.into())); }
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_SETSTANDING_USAGE.into()))?;
            parsed.required(1).map_err(|_| slash_error(messages::gm::GM_SETSTANDING_USAGE.into()))?;
            let owner = parsed.owner(c, 0).await?;
            let character = parsed.target(c, 1).await?;
            let value = parsed.f64(2).ok().filter(|v| (-10.0..=10.0).contains(v))
                .ok_or_else(|| slash_error(messages::gm::GM_SETSTANDING_USAGE.into()))?;
            let world = &*c.world;
            c.db.transaction(
                |tx, outbox| Box::pin(async move {
                    standings::set(tx, character, owner, value).await?;
                    standings::notify_set(outbox, character, owner, value);
                    Ok(())
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            tracing::info!(gm = me, character, owner, value, reason = args.get(3).map(String::as_str).unwrap_or(""), "GM standing set");
            Ok(messages::gm::gm_setstanding(owner, character, value))
        }
        "lp" => {
            if args.len() > 1 { return Err(slash_error(messages::gm::GM_LP_USAGE.into())); }
            if !args.is_empty() { parsed.required(0).map_err(|_| slash_error(messages::gm::GM_LP_USAGE.into()))?; }
            let target = if args.is_empty() { me } else { parsed.target(c, 0).await? };
            let mut rows = standings::lp_balances(&c.db, target).await?;
            rows.sort_unstable_by_key(|row| row.0);
            Ok(messages::gm::gm_lp_list(target, &rows))
        }
        "addlp" | "setlp" => {
            if args.len() != 2 { return Err(slash_error(messages::gm::GM_LP_CHANGE_USAGE.into())); }
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_LP_CHANGE_USAGE.into()))?;
            let corp = parsed.owner(c, 0).await?;
            if c.sde.npc_corporations.get(corp).is_none() && corps::corporation_line(&c.db, &c.sde, corp).await?.is_none() {
                return Err(slash_error(messages::gm::GM_LP_CORPORATION_REQUIRED.into()));
            }
            let amount = parsed.i64(1).map_err(|_| slash_error(messages::gm::GM_LP_CHANGE_USAGE.into()))?;
            let set_mode = command == "setlp";
            let fact_ship = c.session.int("shipid").filter(|id| *id > 0);
            let fact_system = c.session.int("solarsystemid2").filter(|id| *id > 0);
            let fact_station = c.session.int("stationid").filter(|id| *id > 0);
            let world = &*c.world;
            let balance = c.db.transaction(
                |tx, outbox| Box::pin(async move {
                    let before = standings::lp_balance(tx, me, corp).await?;
                    let delta = if set_mode { amount.checked_sub(before) } else { Some(amount) }
                        .ok_or_else(|| slash_error(messages::gm::GM_LP_BALANCE_INVALID.into()))?;
                    before.checked_add(delta).filter(|v| *v >= 0)
                        .ok_or_else(|| slash_error(messages::gm::GM_LP_BALANCE_INVALID.into()))?;
                    standings::add_lp(tx, outbox, me, corp, delta, standings::LoyaltyMutation {
                        reason: crate::lua::events::resources::LoyaltyReason::GmAdjustment,
                        occurred_at: crate::client::filetime_now(),
                        context: crate::lua::events::resources::FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: fact_ship, ship_type_id: None, system_id: fact_system, station_id: fact_station, origin: crate::lua::events::resources::FactOrigin::Gm },
                    }).await?;
                    standings::lp_balance(tx, me, corp).await
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            Ok(messages::gm::gm_lp_changed(corp, balance))
        }
        _ => Err(slash_error(messages::gm::GM_SOCIAL_USAGE.into())),
    }
}
