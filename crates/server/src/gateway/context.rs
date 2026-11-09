use crate::{config::Config, dao::characters::persistence, db::Db, services::identity::Identity};
use super::{Caller, error::{FailureCode, GatewayError}};

pub struct HandlerContext<'a> {
    pub db: &'a Db,
    pub config: &'a Config,
    pub world: &'a crate::space::World,
    pub sde: &'a crate::sde::Sde,
    pub caller: Caller,
    pub identity: Identity,
}
impl<'a> HandlerContext<'a> {
    pub async fn for_request(gateway: &'a super::Gateway, caller: Caller) -> Result<Self, GatewayError> {
        let user = match caller.user_id {
            None => None,
            Some(id) if id > 0 => Some(id),
            Some(_) => return Err(GatewayError::refusal(FailureCode::Unauthorized, infinity_core::messages::gateway::UNAUTHORIZED)),
        };
        let character_id = caller.character_id;
        if let Some(character) = character_id {
            let user = user.ok_or_else(|| GatewayError::refusal(FailureCode::Unauthorized, infinity_core::messages::gateway::UNAUTHORIZED))?;
            let character = (character > 0).then_some(character).ok_or_else(|| GatewayError::refusal(FailureCode::Unauthorized, infinity_core::messages::gateway::UNAUTHORIZED))?;
            if !persistence::owned(&gateway.db, user, character).await? {
                return Err(GatewayError::refusal(FailureCode::Forbidden, infinity_core::messages::gateway::FORBIDDEN));
            }
        }
        let online = character_id.and_then(|id| gateway.world.online_session(id));
        let identity = Identity {
            char_id: character_id.unwrap_or_default(),
            ship_id: online.as_ref().and_then(|s| s.ship_id),
            corp_id: online.as_ref().and_then(|s| s.corporation_id).unwrap_or_default(),
            station_id: online.as_ref().and_then(|s| s.station_id),
            system_id: online.as_ref().and_then(|s| s.system_id),
        };
        Ok(Self { db: &gateway.db, config: gateway.config.as_ref(), world: &gateway.world, sde: gateway.world.sde(), caller, identity })
    }
    pub fn require_user(&self) -> Result<i64, GatewayError> {
        self.caller.user_id.filter(|id| *id > 0).ok_or_else(|| GatewayError::refusal(FailureCode::Unauthorized, infinity_core::messages::gateway::UNAUTHORIZED))
    }
    pub fn require_character(&self) -> Result<&Identity, GatewayError> {
        if self.caller.character_id.is_none() { return Err(GatewayError::refusal(FailureCode::Unauthorized, infinity_core::messages::gateway::UNAUTHORIZED)); }
        Ok(&self.identity)
    }
    pub fn require_online(&self) -> Result<Identity, GatewayError> {
        let identity = self.require_character()?;
        let online = self.world.online_session(identity.char_id)
            .filter(|session| session.user_id == self.caller.user_id && session.character_id == identity.char_id)
            .ok_or_else(|| GatewayError::refusal(FailureCode::Unauthorized, infinity_core::messages::gateway::UNAUTHORIZED))?;
        Ok(Identity { char_id: online.character_id, ship_id: online.ship_id, corp_id: online.corporation_id.unwrap_or_default(), station_id: online.station_id, system_id: online.system_id })
    }
}

#[cfg(test)] mod tests;
