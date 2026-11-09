//! PLEX vault balance for `request_messenger.py:get_balance_request`.
//! Reads account balances through the DAO and descriptors, not World; this
//! handler provides balance only, not vault transactions.
use super::error::GatewayError;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use infinity_core::consts::PLEX_HUNDREDTHS;
use crate::dao::accounts::balances;
use super::{context::HandlerContext, message::{response_for, child, set}};

/// The account's PLEX vault (crate::services::store), in hundredths of a PLEX.
pub(super) async fn plex_balance(ctx: &HandlerContext<'_>) -> Result<DynamicMessage, GatewayError> {
    let mut rsp = response_for("eve_public.plex.vault.api.BalanceRequest").map_err(GatewayError::Internal)?;
    let user = ctx.require_user()?;
    let plex = balances::plex_balance(ctx.db, user).await?;
    let mut balance = child(&rsp.descriptor(), "balance").map_err(GatewayError::Internal)?;
    set(&mut balance, "total_in_cents", PV::I64(plex * PLEX_HUNDREDTHS)).map_err(GatewayError::Internal)?;
    set(&mut rsp, "balance", PV::Message(balance)).map_err(GatewayError::Internal)?;
    Ok(rsp)
}
