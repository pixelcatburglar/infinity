//! Character names for `character_pb2.py:GetNamesByUserRequest`.
//! Reads character names through the characters persistence DAO and descriptors,
//! not World; only the caller's account is queried.
use anyhow::{Context, Result};
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::dao::characters::persistence;
use super::{Gateway, Caller, resources::db, message::{response_for, child, identifier, set}};

/// The account's characters (character rename search).
pub(super) async fn names_by_user(gw: &Gateway, caller: &Caller) -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.character.GetNamesByUserRequest")?;
    let user = caller.user_id.context("no user")?;
    let rows = persistence::names(db(gw)?, user).await?;
    let mut out = vec![];
    for r in rows {
        let mut entry = child(&rsp.descriptor(), "characters")?;
        let id = identifier(&entry.descriptor(), "identifier", PV::U32(r.character_id as u32))?;
        set(&mut entry, "identifier", PV::Message(id))?;
        set(&mut entry, "name", PV::String(r.name))?;
        out.push(PV::Message(entry));
    }
    set(&mut rsp, "characters", PV::List(out))?;
    Ok(rsp)
}
