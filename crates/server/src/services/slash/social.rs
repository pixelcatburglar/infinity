//! GM standing and loyalty-point controls, using standings service notifications.
//! Standings and balances are persisted in the owning service's game tables.
//! Every standing write goes through the shared mutation writer, so the value,
//! its history row and the target's `OnStandingSet` commit together.
//! Loyalty writes go through the shared loyalty administration: absolute sets
//! replace only the balance observed in the same transaction, so a concurrent
//! LP change is refused instead of folded into the target. EverMarks are the
//! Heraldry-issuer balance on the caller or its real database corporation.
use super::{slash_error, Client, CallError};
use infinity_core::messages;
use crate::{db::outbox::dispatch_world, services::{corps, identity::Identity, standings}};

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
        "maxagentstandings" | "fullstandings" => {
            // The reference command treats the whole tail as the target: one
            // token is used directly, more join back into the complete name.
            let target;
            let text = match args {
                [] => None,
                [one] => Some(one.as_str()),
                many => { target = many.join(" "); Some(target.as_str()) }
            };
            let character = match text {
                None => me,
                Some(name) => super::args::target(c, name).await?,
            };
            let summary = standings::gm::max_agent_standings(&c.db, &c.world, &c.sde, me, character).await?;
            tracing::info!(gm = me, character, "GM agent standings raised");
            Ok(messages::gm::gm_agent_standings(character, summary.corporations, summary.factions, summary.changed, summary.already_full))
        }
        "setstanding" => {
            if !(3..=4).contains(&args.len()) { return Err(slash_error(messages::gm::GM_SETSTANDING_USAGE.into())); }
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_SETSTANDING_USAGE.into()))?;
            parsed.required(1).map_err(|_| slash_error(messages::gm::GM_SETSTANDING_USAGE.into()))?;
            let owner = parsed.owner(c, 0).await?;
            let character = parsed.target(c, 1).await?;
            let value = parsed.f64(2).ok().filter(|v| (-standings::STANDING_LIMIT..=standings::STANDING_LIMIT).contains(v))
                .ok_or_else(|| slash_error(messages::gm::GM_SETSTANDING_USAGE.into()))?;
            let reason = args.get(3).map(String::as_str).unwrap_or("");
            // The transaction may retry its closure, so the cause borrows an
            // owned reason cloned per attempt rather than `args`.
            let reason: std::sync::Arc<str> = reason.into();
            let occurred_at = crate::client::filetime_now();
            let world = &*c.world;
            c.db.transaction(
                |tx, outbox| {
                    let reason = reason.clone();
                    Box::pin(async move {
                        standings::mutation::apply_tx(tx, outbox, character,
                            &[standings::mutation::Change { from_id: owner, operation: standings::mutation::Operation::Set(value) }],
                            standings::mutation::Mutation { occurred_at, cause: standings::mutation::Cause::Slash { actor: me, reason: reason.as_ref() } }).await
                    })
                },
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            tracing::info!(gm = me, character, owner, value, reason = reason.as_ref(), "GM standing set");
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
        "evermarks" | "corpevermarks" => {
            if !args.is_empty() { return Err(slash_error(messages::gm::GM_EVERMARKS_USAGE.into())); }
            let scope = if command == "evermarks" { standings::loyalty::Scope::Character } else { standings::loyalty::Scope::Corporation };
            let balance = standings::loyalty::gm_balance(&c.db, &c.sde, me, scope).await?;
            Ok(messages::gm::gm_evermarks_balance(balance.owner_id, matches!(scope, standings::loyalty::Scope::Corporation), balance.amount))
        }
        "addevermarks" | "setevermarks" | "addcorpevermarks" | "setcorpevermarks" => {
            let set = command.starts_with("set");
            let usage = if set { messages::gm::GM_EVERMARKS_SET_USAGE } else { messages::gm::GM_EVERMARKS_ADD_USAGE };
            if args.len() != 1 { return Err(slash_error(usage.into())); }
            let amount = parsed.i64(0).map_err(|_| slash_error(usage.into()))?;
            let corporation = matches!(command, "addcorpevermarks" | "setcorpevermarks");
            let scope = if corporation { standings::loyalty::Scope::Corporation } else { standings::loyalty::Scope::Character };
            let change = if set { standings::loyalty::Change::Set(amount) } else { standings::loyalty::Change::Add(amount) };
            let balance = standings::loyalty::gm_change(&c.db, &c.world, &c.sde, Identity::of_client(c), infinity_core::consts::CORP_HERALDRY, scope, change).await?;
            Ok(messages::gm::gm_evermarks_balance(balance.owner_id, corporation, balance.amount))
        }
        "addlp" | "setlp" => {
            if args.len() != 2 { return Err(slash_error(messages::gm::GM_LP_CHANGE_USAGE.into())); }
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_LP_CHANGE_USAGE.into()))?;
            let corp = parsed.owner(c, 0).await?;
            if c.sde.npc_corporations.get(corp).is_none() && corps::corporation_line(&c.db, &c.sde, corp).await?.is_none() {
                return Err(slash_error(messages::gm::GM_LP_CORPORATION_REQUIRED.into()));
            }
            let amount = parsed.i64(1).map_err(|_| slash_error(messages::gm::GM_LP_CHANGE_USAGE.into()))?;
            let change = if command == "setlp" { standings::loyalty::Change::Set(amount) } else { standings::loyalty::Change::Add(amount) };
            let balance = standings::loyalty::gm_change(&c.db, &c.world, &c.sde, Identity::of_client(c), corp, standings::loyalty::Scope::Character, change).await?;
            Ok(messages::gm::gm_lp_changed(corp, balance.amount))
        }
        _ => Err(slash_error(messages::gm::GM_SOCIAL_USAGE.into())),
    }
}
