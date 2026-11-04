//! Ship SKIN gateway messages for shipSkinStateRequestMessenger.py and
//! shipSkinStateNoticeMessenger.py. State comes from the shared domain projection;
//! third-party skins are not produced.
use std::collections::BTreeSet;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::{client::filetime_now, services::{ship_cosmetics_mgr::state::{self, State}, CallError}};
use super::{context::HandlerContext, error::{FailureCode, GatewayError}, message::{child, identifier, message, msg_field, response_for, set}, notices::{GatewayNotice, producer_error}, protocol::target_group::Target};

const GET: &str = "eve_public.cosmetic.ship.api.GetRequest";
const ALL: &str = "eve_public.cosmetic.ship.api.GetAllInBubbleRequest";
const SET: &str = "eve_public.cosmetic.ship.api.SetNotice";
const SET_ALL: &str = "eve_public.cosmetic.ship.api.SetAllInBubbleNotice";

fn encode(state: &State) -> anyhow::Result<DynamicMessage> {
    let mut out = DynamicMessage::new(message("eve_public.cosmetic.ship.State").ok_or_else(|| anyhow::anyhow!("missing ship state descriptor"))?);
    let ship = identifier(&out.descriptor(), "ship", PV::U64(u64::try_from(state.ship_id)?))?;
    let character = identifier(&out.descriptor(), "character", PV::U32(u32::try_from(state.character_id)?))?;
    set(&mut out, "ship", PV::Message(ship))?;
    set(&mut out, "character", PV::Message(character))?;
    let mut skin = child(&out.descriptor(), "skin")?;
    if let Some(id) = state.skin_id {
        let mut first = child(&skin.descriptor(), "firstparty")?;
        let id = identifier(&first.descriptor(), "identifier", PV::U64(u64::try_from(id)?))?;
        set(&mut first, "identifier", PV::Message(id))?;
        set(&mut skin, "firstparty", PV::Message(first))?;
    } else { set(&mut skin, "no_skin", PV::Bool(true))?; }
    set(&mut out, "skin", PV::Message(skin))?;
    Ok(out)
}
fn character_target(character: i64) -> Result<Target, CallError> {
    u32::try_from(character).map(Target::Character)
        .map_err(|error| producer_error(GatewayError::Internal(error.into())))
}

pub(crate) fn notice(state: &State, characters: &[i64]) -> Result<Vec<GatewayNotice>, CallError> {
    let targets: BTreeSet<_> = characters.iter().copied().filter(|id| *id > 0).collect();
    let mut result = Vec::with_capacity(targets.len());
    let mut iter = targets.into_iter();
    let Some(first) = iter.next() else { return Ok(Vec::new()) };
    let mut payload = DynamicMessage::new(message(SET).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing SetNotice descriptor")))?);
    set(&mut payload, "state", PV::Message(encode(state).map_err(CallError::Internal)?)).map_err(CallError::Internal)?;
    let first = GatewayNotice::new(character_target(first)?, &payload).map_err(producer_error)?;
    result.push(first);
    for id in iter { result.push(result[0].retarget(character_target(id)?).map_err(producer_error)?); }
    Ok(result)
}

pub(crate) fn all_notice(states: &[State], character: i64) -> Result<GatewayNotice, CallError> {
    let mut payload = DynamicMessage::new(message(SET_ALL).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing SetAllInBubbleNotice descriptor")))?);
    let values = states.iter().map(|s| encode(s).map(PV::Message)).collect::<anyhow::Result<Vec<_>>>().map_err(CallError::Internal)?;
    set(&mut payload, "state", PV::List(values)).map_err(CallError::Internal)?;
    GatewayNotice::new(character_target(character)?, &payload).map_err(producer_error)
}

pub(super) async fn handle(ctx: &HandlerContext<'_>, name: &str, request: &DynamicMessage) -> Result<DynamicMessage, GatewayError> {
    let identity = ctx.require_online()?;
    let mut response = response_for(name).map_err(GatewayError::Internal)?;
    let map_error = |error: CallError| GatewayError::Internal(anyhow::anyhow!("ship state projection failed: {error:?}"));
    if name == GET {
        let ship = msg_field(request, "ship").and_then(|m| m.get_field_by_name("sequential").and_then(|v| v.as_u64())).and_then(|id| i64::try_from(id).ok()).filter(|id| *id > 0)
            .ok_or_else(|| GatewayError::refusal(FailureCode::BadRequest, crate::messages::gateway::INVALID_REQUEST))?;
        let state = state::get(ctx.db, ctx.world, &identity, ship, filetime_now()).await.map_err(map_error)?
            .ok_or_else(|| GatewayError::refusal(FailureCode::NotFound, crate::messages::gateway::NOT_FOUND))?;
        set(&mut response, "state", PV::Message(encode(&state).map_err(GatewayError::Internal)?)).map_err(GatewayError::Internal)?;
    } else if name == ALL {
        let states = state::all(ctx.db, ctx.world, ctx.sde, &identity, filetime_now()).await.map_err(map_error)?;
        let values = states.iter().map(|s| encode(s).map(PV::Message)).collect::<anyhow::Result<Vec<_>>>().map_err(GatewayError::Internal)?;
        set(&mut response, "states", PV::List(values)).map_err(GatewayError::Internal)?;
    }
    Ok(response)
}

pub(super) fn validate() -> anyhow::Result<()> {
    for name in ["eve_public.cosmetic.ship.State", SET, SET_ALL] { anyhow::ensure!(message(name).is_some(), "missing descriptor {name}"); }
    let sample = State { ship_id: 1, character_id: 1, skin_id: Some(1) };
    let encoded = encode(&sample)?;
    let mut request = DynamicMessage::new(message(GET).ok_or_else(|| anyhow::anyhow!("missing GetRequest descriptor"))?);
    let ship = identifier(&request.descriptor(), "ship", PV::U64(1))?;
    set(&mut request, "ship", PV::Message(ship))?;
    let mut response = response_for(GET)?;
    set(&mut response, "state", PV::Message(encoded.clone()))?;
    let mut response_all = response_for(ALL)?;
    set(&mut response_all, "states", PV::List(vec![PV::Message(encoded.clone())]))?;
    let mut single = DynamicMessage::new(message(SET).unwrap());
    set(&mut single, "state", PV::Message(encoded.clone()))?;
    let mut all = DynamicMessage::new(message(SET_ALL).unwrap());
    set(&mut all, "state", PV::List(vec![PV::Message(encoded)]))?;
    encode(&State { skin_id: None, ..sample })?;
    Ok(())
}

#[cfg(test)] mod tests;
