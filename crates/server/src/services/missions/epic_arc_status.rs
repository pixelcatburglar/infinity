//! Read-only epic-arc agent availability for the selected character and agent ID.
//! `eve/client/script/ui/station/agents/agentUtil.py:IsAgentAvailable` and
//! `GetAgentUnavailableReason`, and `eve/client/script/ui/shared/dockedUI/agents.py:
//! find_available_and_unavailable_agents` consume a boolean. Uses retail agents,
//! loaded SDE arc steps and the character's persisted missions and progress.
//! Replay and recovery rules are unchanged.
use super::*;

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    if call.method != "AgentHasEpicMissionsForCharacter" { return None; }
    Some(async {
        let agent_id = call.args.first().and_then(Value::as_int)
            .ok_or_else(|| super::super::notify_error(messages::missions::AGENT_REQUIRED))?;
        c.session.int("charid").ok_or_else(|| super::super::notify_error(messages::missions::CHARACTER_REQUIRED))?;
        if agent(agent_id).is_none() || !is_arc_agent(c, agent_id).await? { return Ok(Value::Bool(false)); }
        if missions_of(c, Some(agent_id)).await?.iter().any(|m| m.arc_id.is_some() && (m.state == STATE_OFFERED || m.state == STATE_ACCEPTED)) {
            return Ok(Value::Bool(true));
        }
        Ok(Value::Bool(next_arc_step(c, agent_id).await?.is_some()))
    }.await)
}

#[cfg(test)]
mod tests;
