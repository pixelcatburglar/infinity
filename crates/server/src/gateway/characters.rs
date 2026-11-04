//! Character names for `character_pb2.py:GetNamesByUserRequest`.
//! Reads character names through the characters persistence DAO and descriptors,
//! not World; only the caller's account is queried.
use super::error::GatewayError;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::dao::characters::persistence;
use super::{context::HandlerContext, message::{response_for, child, identifier, set}};

/// The account's characters (character rename search).
pub(super) async fn names_by_user(ctx: &HandlerContext<'_>) -> Result<DynamicMessage, GatewayError> {
    let mut rsp = response_for("eve_public.character.GetNamesByUserRequest").map_err(GatewayError::Internal)?;
    let user = ctx.require_user()?;
    let rows = persistence::names(ctx.db, user).await?;
    let mut out = vec![];
    for r in rows {
        let mut entry = child(&rsp.descriptor(), "characters").map_err(GatewayError::Internal)?;
        let id = identifier(&entry.descriptor(), "identifier", PV::U32(r.character_id as u32)).map_err(GatewayError::Internal)?;
        set(&mut entry, "identifier", PV::Message(id)).map_err(GatewayError::Internal)?;
        set(&mut entry, "name", PV::String(r.name)).map_err(GatewayError::Internal)?;
        out.push(PV::Message(entry));
    }
    set(&mut rsp, "characters", PV::List(out)).map_err(GatewayError::Internal)?;
    Ok(rsp)
}
