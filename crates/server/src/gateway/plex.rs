//! PLEX vault balance for `request_messenger.py:get_balance_request`.
//! Reads account balances through the DAO and descriptors, not World; this
//! handler provides balance only, not vault transactions.
use anyhow::{Context, Result};
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::{consts::PLEX_HUNDREDTHS, dao::accounts::balances};
use super::{Gateway, Caller, resources::db, message::{response_for, child, set}};

/// The account's PLEX vault (crate::services::store), in hundredths of a PLEX.
pub(super) async fn plex_balance(gw: &Gateway, caller: &Caller) -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.plex.vault.api.BalanceRequest")?;
    let user = caller.user_id.context("no user")?;
    let plex = balances::plex_balance(db(gw)?, user).await?;
    let mut balance = child(&rsp.descriptor(), "balance")?;
    set(&mut balance, "total_in_cents", PV::I64(plex * PLEX_HUNDREDTHS))?;
    set(&mut rsp, "balance", PV::Message(balance))?;
    Ok(rsp)
}
