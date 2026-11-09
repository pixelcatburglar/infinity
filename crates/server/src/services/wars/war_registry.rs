//! Public bound `warRegistry` reads and mutual-war retraction.
//! Mirrors `eve/client/script/ui/services/wars/war_cso.py`; reads `game.wars`.
//! Ally, surrender, aggressive and GM war mutations are not supported.

use infinity_core::messages;
use crate::{client::Client, dao::wars};
use crate::services::{Call, CallResult, db_err, notify_error};
use marshal::Value;
use super::{lifecycle, wire};

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let arity = match call.method {
        "GetNegotiations" => Some(0),
        "GetWars" | "RetractMutualWar" | "GetWarNegotiation" | "RetractWarAllyOffer"
        | "AcceptAllyNegotiation" | "DeclineAllyOffer" | "AcceptSurrender" | "DeclineSurrender" => Some(1),
        "SetOpenForAllies" | "GMActivateDefender" | "GMDeactivateDefender"
        | "GMSetDeclareTime" | "GMSetWarFinishTime" => Some(2),
        "CreateSurrenderNegotiation" | "GMJoinDefender" | "GMExtendAllyContract" => Some(3),
        "CreateWarAllyOffer" => Some(4),
        _ => None,
    };
    if arity.is_some_and(|expected| call.args.len() != expected) {
        return Some(Err(notify_error(messages::wars::INVALID)));
    }
    Some(match call.method {
        "GetWars" => match call.args.first() {
            Some(Value::Int(id)) if *id > 0 => wars::by_owner(&c.db, *id).await.map(|rows| wire::wars(&rows)).map_err(db_err),
            _ => Err(notify_error(messages::wars::INVALID)),
        },
        "GetNegotiations" => Ok(Value::list([])),
        "RetractMutualWar" => match (call.args.first(), c.session.int("charid")) {
            (Some(Value::Int(id)), Some(actor)) if *id > 0 && actor > 0 => lifecycle::retract(&c.db, &c.world, actor, *id).await.map(|()| Value::None),
            _ => Err(notify_error(messages::wars::INVALID)),
        },
        "GetWarNegotiation" | "CreateSurrenderNegotiation" | "AcceptSurrender" | "DeclineSurrender" => Err(notify_error(messages::wars::SURRENDER_UNAVAILABLE)),
        "CreateWarAllyOffer" | "RetractWarAllyOffer" | "AcceptAllyNegotiation" | "DeclineAllyOffer" | "SetOpenForAllies" => Err(notify_error(messages::wars::ALLIES_UNAVAILABLE)),
        "DeclareWar" | "DeclareWarAgainst" => Err(notify_error(messages::wars::AGGRESSIVE_UNAVAILABLE)),
        "GMJoinDefender" | "GMActivateDefender" | "GMDeactivateDefender" | "GMExtendAllyContract" | "GMSetDeclareTime" | "GMSetWarFinishTime" => Err(notify_error(messages::wars::AUTHORITY)),
        _ => return None,
    })
}
