//! Agents and kill missions (eve/client/script/ui/station/agents).
//!
//! The client binds one `agentMgr` moniker per agent (eveMoniker.GetAgent) and talks
//! through `DoAction(actionID) -> ((agentSays, [(actionID, button)]), lastActionInfo)`,
//! then renders the briefing (GetMissionBriefingInfo), objectives
//! (GetMissionObjectiveInfo) and journal bookmarks (GetMyJournalDetails).
//!
//! Agents come from the retail agent list (crate::retail). Basic agents offer the
//! shared SDE's kill missions whose dungeon difficulty matches the agent's level.
//! `GetDisabledMissions` returns an empty list. Agent info uses the retail agent
//! list; unbound or non-agent info requests return None.
//!
//! Epic arcs in the shared SDE chain missions across agents: an arc's first step is
//! offered by its agent to anyone who hasn't begun it, and each completed step unlocks
//! the next ones (an agent-interaction step unlocks the branch the pilot chose). Steps are
//! kill missions, talk-to-agent and courier missions (finished at the next step's agent;
//! courier cargo goes to the pilot's hangar on accepting), trade missions (bring the
//! goods back) and agent interactions (a choice of answers). The live server's dungeon layouts never
//! shipped with the client, so a site is a deadspace point above a planet's surface in the
//! agent's system, spawned with that dungeon faction's mission NPCs on first approach.
//! The objective completes once the actor's spawned NPC group is cleared. Site
//! records and group state are held in memory. Startup moves legacy buried mission sites
//! clear of planets.
//! Operators configure basic mission rewards, site placement and NPC spawn distances.
//! Epic arc rewards continue to come from mission data.
mod agent_mgr;
mod epic_arc_status;
mod gm;
mod mechanics;
mod mission_tracker_mgr;
mod sites;

use mechanics::*;
pub(crate) use gm::force_complete;
use sites::{despawn, ensure_site, objective_done};
#[cfg(test)]
use sites::{group_has_class, spawn_classes};
use super::{combat, inventory, items, off_surface, ship, site_position, standings};


use super::{db_err, keyval, rand01, rand_dir, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{FLAG_CARGO, FLAG_HANGAR, IX_STACK_SIZE, TYPE_SOLAR_SYSTEM};
use crate::messages;
use crate::space::{dist, now_ms, V3};
use marshal::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicI64;
use std::sync::{Mutex, OnceLock};
use tracing::info;

// appConst.agentMissionState*
const STATE_OFFERED: i32 = 1;
const STATE_ACCEPTED: i32 = 2;
// appConst.agentDialogueButton*
const BTN_VIEW: i64 = 1;
const BTN_REQUEST: i64 = 2;
const BTN_ACCEPT: i64 = 3;
const BTN_COMPLETE: i64 = 6;
const BTN_DECLINE: i64 = 9;
const BTN_QUIT: i64 = 11;
// Our action IDs (DoAction's argument).
const ACT_REQUEST: i64 = 1;
const ACT_VIEW: i64 = 2;
const ACT_ACCEPT: i64 = 3;
const ACT_DECLINE: i64 = 4;
const ACT_COMPLETE: i64 = 5;
const ACT_QUIT: i64 = 6;
/// Hand in a talk-to-agent or courier mission another agent gave us.
const ACT_DELIVER: i64 = 7;
/// Agent-interaction answers: ACT_OPTION + option index.
const ACT_OPTION: i64 = 10;
const AGENT_TYPE_BASIC: i64 = 2;
const TYPE_CREDITS: i64 = 29;
/// Owner of mission NPCs from dungeons without a faction (Guristas Pirates).
const GENERIC_NPC_CORP: i64 = 1000127;
/// A ship this close to a site's centre (m) is in it.
const SITE_RADIUS: f64 = 100_000.0;
/// Server design: fallback station radius in metres when its ball is unavailable.
const FALLBACK_STATION_RADIUS: f64 = 5000.0;
/// Temporary item IDs for spawned NPCs.
pub(super) static NEXT_NPC_ID: AtomicI64 = AtomicI64::new(9_000_000_000_000);

// ---------------------------------------------------------------- agents

pub struct Agent {
    pub id: i64,
    pub type_id: i64,
    pub level: i64,
    pub station_id: i64,
    pub system_id: i64,
    pub corp_id: i64,
    pub faction_id: i64,
}

/// The retail agent list: Rowset lines `[agentID, agentTypeID, divisionID, level, stationID,
/// bloodlineID, corporationID, gender, isLocatorAgent, factionID, solarSystemID]`.
fn agents() -> &'static HashMap<i64, Agent> {
    static AGENTS: OnceLock<HashMap<i64, Agent>> = OnceLock::new();
    AGENTS.get_or_init(|| {
        let mut out = HashMap::new();
        let Some(Value::Instance(_, state)) = crate::retail::cached_call("agentMgr", "GetAgents", &[]) else { return out };
        for line in state.get("lines").and_then(|l| l.items()).unwrap_or_default() {
            let Some(f) = line.items() else { continue };
            let n = |i: usize| f.get(i).and_then(|v| v.as_int()).unwrap_or(0);
            out.insert(n(0), Agent { id: n(0), type_id: n(1), level: n(3), station_id: n(4), system_id: n(10), corp_id: n(6), faction_id: n(9) });
        }
        out
    })
}

pub fn agent(id: i64) -> Option<&'static Agent> {
    agents().get(&id)
}

// ---------------------------------------------------------------- missions

/// What a mission asks for (its content template).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Kill,
    Talk,
    Courier,
    Trade,
    Interaction,
}

impl Kind {
    fn of_template(template: &str) -> Kind {
        match template.rsplit('_').next().unwrap_or("") {
            t if t.ends_with("TalkToAgentMission") => Kind::Talk,
            t if t.ends_with("CourierMission") => Kind::Courier,
            t if t.ends_with("TradeMission") => Kind::Trade,
            t if t.ends_with("AgentInteractionMission") => Kind::Interaction,
            _ => Kind::Kill,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Kind::Kill => "kill",
            Kind::Talk => "talk",
            Kind::Courier => "courier",
            Kind::Trade => "trade",
            Kind::Interaction => "interaction",
        }
    }

    fn of_code(code: &str) -> Kind {
        match code {
            "talk" => Kind::Talk,
            "courier" => Kind::Courier,
            "trade" => Kind::Trade,
            "interaction" => Kind::Interaction,
            _ => Kind::Kill,
        }
    }

    /// The journal's mission type label (UI/Agents/MissionTypes/*).
    fn journal_label(self, arc: bool) -> &'static str {
        match (self, arc) {
            (Kind::Kill, false) => "UI/Agents/MissionTypes/Encounter",
            (Kind::Kill, true) => "UI/Agents/MissionTypes/EpicArcEncounter",
            (Kind::Talk, _) => "UI/Agents/MissionTypes/EpicArcTalkToAgent",
            (Kind::Courier, true) => "UI/Agents/MissionTypes/EpicArcCourier",
            (Kind::Courier, false) => "UI/Agents/MissionTypes/Courier",
            (Kind::Trade, true) => "UI/Agents/MissionTypes/EpicArcTrade",
            (Kind::Trade, false) => "UI/Agents/MissionTypes/Trade",
            (Kind::Interaction, _) => "UI/Agents/MissionTypes/EpicArcAgentInteraction",
        }
    }
}

struct Mission {
    agent_id: i64,
    mission_id: i32,
    name_id: i64,
    briefing_id: i64,
    dungeon_id: i32,
    faction_id: i64,
    state: i32,
    system_id: i64,
    pos: V3,
    reward_isk: i64,
    reward_lp: i64,
    accepted_at: Option<i64>,
    kind: Kind,
    /// The epic arc this mission is a step of.
    arc_id: Option<i64>,
    /// Where a talk-to-agent or courier mission is handed in (None: the giver).
    dest_agent: Option<i64>,
    /// (typeID, quantity) a courier mission carries or a trade mission wants.
    cargo: Option<(i32, i32)>,
}

impl Mission {
    fn hand_in_agent(&self) -> i64 {
        self.dest_agent.unwrap_or(self.agent_id)
    }
}
pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "agentMgr" => agent_mgr::dispatch(c, call).await,
        "epicArcStatus" => epic_arc_status::dispatch(c, call).await,
        "missionTrackerMgr" => mission_tracker_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
