//! Mission state, offers, rewards and arc mechanics; completion commits before dispatching
//! effects. The client renders dungeon objectives in
//! `agentinteraction/objectivesteps.py:103-104,123-125,137-142`.
//! Mission and dungeon candidates, messages and placement use the shared SDE.
//! Mission, character, item, loyalty and standing state is
//! stored in the game database; site simulation is separate.
//! Basic mission rewards and placement are operator policy, not a client-provided generator;
//! complete live-server mission layouts and mechanics are not reproduced.
//! Committed mission completions and reward wallet changes queue Lua events.
//! Every mission ending — completion, decline or quit — journals its standing
//! change inside the same transaction as the mission row and arc state through the
//! shared `services::standings::mutation` writer, so standing value, history row,
//! mission removal and arc updates commit or roll back together; `despawn` and
//! `OnAgentMissionChange` only run after that commit.
//! Offers freeze their integer gross ISK and LP on the mission row through
//! `reward_quote`: the character's effective `negotiationPercentage` (Negotiation)
//! decides the ISK factor, and the issuing agent's NPC division picks the
//! Connections skill whose effective level adds a tenth of the base LP each.
//! Saved amounts are never repriced. Completion reads the pilot's current
//! Social/booster standing-gain multiplier from one Fit before the market lock,
//! then only scales the positive configured gain rates inside the settlement
//! transaction.
use super::*;
use crate::services::fit::{Fit, Key};
use infinity_core::consts::dogma::ATTR_SKILL_LEVEL;

/// SDE dgmAttributeTypes.negotiationPercentage: the character's effective agent-pay
/// factor; Negotiation (invTypes 3356) raises it through effects 403/404.
const ATTR_MISSION_PAY_FACTOR: i32 = 355;
/// NPC division (client npccorporationdivisions) selects the Connections skill (SDE
/// invTypes) whose effective level adds one tenth of the base LP per level.
const DIVISION_LP_SKILL: [(i64, i32); 3] = [(22, 3894), (23, 3893), (24, 3895)];
/// The whole-LP divisor: base LP times (DIVISOR + level) over DIVISOR.
const LP_LEVEL_DIVISOR: i128 = 10;

/// One integer gross quote frozen into an offer: ISK by the effective negotiation
/// pay factor (floored once, exact integers when the factor is one), LP by the
/// division Connections skill's effective level (a tenth of base each). Unknown
/// divisions keep the base LP. Negative or non-finite inputs are refused, never
/// truncated into a written reward.
fn reward_quote(fit: &Fit, division: i64, (isk, lp): (i64, i64)) -> Result<(i64, i64), CallError> {
    let isk = quote_isk(fit.value(Key::Char, ATTR_MISSION_PAY_FACTOR), isk)?;
    let lp = match DIVISION_LP_SKILL.iter().find(|(d, _)| *d == division) {
        Some(&(_, skill)) => quote_lp(effective_level(fit, skill)?, lp)?,
        None => quote_lp(0, lp)?,
    };
    Ok((isk, lp))
}

/// An effective Connections level; non-integral or negative values are data
/// faults, not quotes.
fn effective_level(fit: &Fit, skill: i32) -> Result<i64, CallError> {
    let level = fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL);
    if !level.is_finite() || level < 0.0 || level.fract() != 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid effective skill level {level}")));
    }
    Ok(level as i64)
}

/// `base` scaled by the negotiated factor, floored once; identity keeps the
/// exact integer and an infinite or out-of-range result is refused.
fn quote_isk(factor: f64, base: i64) -> Result<i64, CallError> {
    if base < 0 || !factor.is_finite() || factor <= 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid mission ISK quote")));
    }
    if factor == 1.0 { return Ok(base); }
    let scaled = (base as f64 * factor).floor();
    if !(i64::MIN as f64..-(i64::MIN as f64)).contains(&scaled) {
        return Err(CallError::Internal(anyhow::anyhow!("mission ISK reward out of range")));
    }
    Ok(scaled as i64)
}

/// `base` plus `level` tenths of itself in integer arithmetic.
fn quote_lp(level: i64, base: i64) -> Result<i64, CallError> {
    if base < 0 || level < 0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid mission LP quote")));
    }
    let scaled = i128::from(base)
        .checked_mul(LP_LEVEL_DIVISOR + i128::from(level))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("mission LP reward out of range")))?
        / LP_LEVEL_DIVISOR;
    i64::try_from(scaled).map_err(|_| CallError::Internal(anyhow::anyhow!("mission LP reward out of range")))
}
/// (ISK, LP) rewards by agent level.
fn rewards(level: i64) -> (i64, i64) {
    let index = if (1..=4).contains(&level) { (level - 1) as usize } else { 4 };
    let reward = &crate::config::game().missions.rewards[index];
    (reward.isk, reward.lp)
}

fn mission_from(r: crate::dao::missions::MissionRow, data: &crate::sde::missions::Mission) -> Mission {
    Mission {
        agent_id: r.agent_id,
        mission_id: r.mission_id,
        name_id: data.name_id.unwrap_or(0),
        briefing_id: data.message("messages.mission.briefing").unwrap_or(0),
        dungeon_id: r.dungeon_id,
        faction_id: r.faction_id,
        state: r.state,
        system_id: r.system_id,
        pos: [r.x, r.y, r.z],
        reward_isk: r.reward_isk,
        reward_lp: r.reward_lp,
        accepted_at: r.accepted_at,
        kind: Kind::of_code(&r.kind),
        arc_id: r.arc_id,
        dest_agent: r.dest_agent,
        cargo: r.cargo_type.zip(r.cargo_qty),
    }
}

pub(super) async fn missions_of(c: &Client, agent_id: Option<i64>) -> Result<Vec<Mission>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    Ok(crate::dao::missions::missions_of(&c.db, me, agent_id).await.map_err(db_err)?.into_iter()
        .filter_map(|r| c.sde.missions.get(r.mission_id).map(|data| mission_from(r, data))).collect())
}

pub(super) async fn mission_with(c: &Client, agent_id: i64) -> Result<Option<Mission>, CallError> {
    Ok(missions_of(c, Some(agent_id)).await?.into_iter().next())
}

/// A mission message ID of this mission (`messages.mission.accepted.agentsays`...).
pub(super) async fn message(c: &Client, mission_id: i32, key: &str) -> Result<Option<i64>, CallError> {
    let mission = c.sde.missions.get(mission_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing mission {mission_id}")))?;
    Ok(mission.message(key))
}

/// A mission site: a point well off the surface of a random planet of the system.
fn mission_site_position(c: &Client, system: i64) -> V3 {
    let (base, radius) = super::site_position(&c.sde, system).unwrap_or(([0.0; 3], 0.0));
    super::off_surface(base, radius, crate::config::game().missions.site_distance_m)
}

fn eligible_kill(m: &crate::sde::missions::Mission, dungeon: &crate::sde::dungeons::Dungeon, level: i32) -> bool {
    m.template.as_deref() == Some("agent.missionTemplatizedContent_BasicKillMission")
        && dungeon.difficulty == Some(i64::from(level)) && m.has_name
        && m.has_message("messages.mission.briefing")
}

/// Offer one of the build's kill missions at the agent's level, sited in the agent's system.
/// `fit` is the pilot's character snapshot at `at`; the quote it yields is frozen
/// on the row and never recomputed on view, acceptance or completion.
pub(super) async fn offer(c: &Client, agent: &Agent, fit: &Fit, at: i64) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let level = agent.level.clamp(1, 5) as i32;
    let candidates: Vec<_> = c.sde.missions.iter().filter_map(|m| {
        let dungeon = c.sde.dungeons.get(i64::from(m.dungeon_id?))?;
        eligible_kill(m, dungeon, level).then_some((m, dungeon))
    }).collect();
    let (pick, dungeon) = candidates.get((rand01() * candidates.len() as f64) as usize)
        .copied().ok_or_else(|| CallError::Internal(anyhow::anyhow!("no level {level} kill missions")))?;
    let dungeon_id = pick.dungeon_id.unwrap_or(0);
    let faction_id = dungeon.faction_id.unwrap_or(0);
    let pos = mission_site_position(c, agent.system_id);
    let (isk, lp) = reward_quote(fit, agent.division_id, rewards(agent.level))?;
    crate::dao::missions::offer(&c.db, &crate::dao::missions::Offer {
        character_id: me, agent_id: agent.id, mission_id: pick.mission_id, dungeon_id,
        faction_id, state: STATE_OFFERED, system_id: agent.system_id, pos,
        reward_isk: isk, reward_lp: lp, offered_at: at,
    }).await.map_err(db_err)?;
    info!(user = c.login.user_id, "agent {}: offered mission {} (dungeon {})", agent.id, pick.mission_id, dungeon_id);
    Ok(())
}

// ---------------------------------------------------------------- epic arcs


pub(super) async fn is_arc_agent(c: &Client, agent_id: i64) -> Result<bool, CallError> {
    Ok(c.sde.missions.steps().any(|s| s.agent == agent_id))
}

/// The arc step this agent has for us next: the start of an arc we haven't begun, or a
/// step our progress has unlocked.
pub(super) async fn next_arc_step(c: &Client, agent_id: i64) -> Result<Option<i32>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = crate::dao::missions::arc_progress(&c.db, me).await.map_err(db_err)?;
    let begun: HashSet<i64> = rows.iter().map(|r| r.arc_id).collect();
    let unlocked: HashSet<i32> = rows.iter().filter(|r| r.state == "unlocked").map(|r| r.mission_id).collect();
    Ok(c.sde.missions.steps().find(|s| s.agent == agent_id && (unlocked.contains(&s.mission_id) || (s.start && !begun.contains(&s.arc)))).map(|s| s.mission_id))
}


/// Offer an epic arc step: its content (kill site, courier cargo, trade goods...) and
/// rewards come from the mission's own data, quoted and frozen like a basic offer.
pub(super) async fn offer_arc(c: &Client, agent: &Agent, mission_id: i32, fit: &Fit, at: i64) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(step) = c.sde.missions.step(mission_id) else { return Ok(()) };
    let r = c.sde.missions.get(mission_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing mission {mission_id}")))?;
    let dungeon_id = r.dungeon_id.unwrap_or(0);
    let faction_id = r.dungeon_id.and_then(|id| c.sde.dungeons.get(i64::from(id))).and_then(|d| d.faction_id).or(r.faction_id).unwrap_or(0);
    let base_isk = if r.reward_type == Some(TYPE_CREDITS as i32) { r.reward_quantity.expect("credit reward quantity is NULL") } else { 0 };
    let base_lp = r.reward_lp.unwrap_or(0);
    let (reward_isk, reward_lp) = reward_quote(fit, agent.division_id, (base_isk, base_lp))?;
    let kind = Kind::of_template(r.template.as_deref().unwrap_or_default());
    // Talk-to-agent and courier steps end at the agent of the step that follows.
    let dest = match kind {
        Kind::Talk | Kind::Courier => step.next.first().and_then(|n| c.sde.missions.step(*n)).map(|s| s.agent).filter(|a| *a != agent.id),
        _ => None,
    };
    let cargo_type = r.cargo_type;
    let cargo_qty: Option<i32> = cargo_type.map(|_| r.cargo_qty.unwrap_or(1).max(1));
    let pos = mission_site_position(c, agent.system_id);
    crate::dao::missions::offer_arc(&c.db, &crate::dao::missions::ArcOffer {
        offer: crate::dao::missions::Offer {
            character_id: me, agent_id: agent.id, mission_id, dungeon_id,
            faction_id, state: STATE_OFFERED, system_id: agent.system_id, pos,
            reward_isk, reward_lp, offered_at: at,
        },
        kind: kind.code(), arc_id: step.arc, dest_agent: dest, cargo_type, cargo_qty,
    }).await.map_err(db_err)?;
    info!(user = c.login.user_id, "agent {}: offered epic arc {} step {mission_id} ({kind:?})", agent.id, step.arc);
    Ok(())
}

/// agentMgr.GetMyEpicArcStatus() -> {arcID: {missionID: (acceptedDate, completedDate,
/// quitDate, nameID)}} (epicArcController._GetMissionStatus).
pub(super) async fn epic_arc_status(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = crate::dao::missions::arc_status(&c.db, me).await.map_err(db_err)?;
    let mut by_arc: Vec<(i64, Vec<(Value, Value)>)> = vec![];
    for r in rows {
        let Some(data) = c.sde.missions.get(r.mission_id) else { continue };
        let arc = r.arc_id;
        let stamp = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
        let status = keyval([("acceptedDate", stamp(r.accepted_at)), ("completedDate", stamp(r.completed_at)), ("quitDate", stamp(r.quit_at)), ("nameID", stamp(data.name_id))]);
        match by_arc.last_mut() {
            Some((a, list)) if *a == arc => list.push((Value::Int(r.mission_id as i64), status)),
            _ => by_arc.push((arc, vec![(Value::Int(r.mission_id as i64), status)])),
        }
    }
    Ok(Value::Dict(by_arc.into_iter().map(|(arc, list)| (Value::Int(arc), Value::Dict(list))).collect()))
}

// ---------------------------------------------------------------- cargo

/// Where a pilot can hand goods in at `station`: its hangar there, or its ship's cargo
/// while docked there.
async fn goods_at(c: &Client, station: i64, type_id: i32) -> Result<Vec<crate::dao::items::loading::Item>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let ship = if c.session.int("stationid") == Some(station) { c.session.int("shipid").unwrap_or(0) } else { 0 };
    crate::services::items::load_items(&c.db, &c.sde, crate::dao::items::loading::ItemQuery::OwnerTypeTwoLocations(me, type_id, station, FLAG_HANGAR, ship, FLAG_CARGO)).await.map_err(db_err)
}

pub(super) async fn has_goods(c: &Client, station: i64, (type_id, qty): (i32, i32)) -> Result<bool, CallError> {
    let available = goods_at(c, station, type_id).await?.iter().try_fold(0_i128, |total, item| {
        total.checked_add(i128::from(crate::services::item_volume::stack_size(item.singleton, item.quantity)))
    }).ok_or_else(|| CallError::Internal(anyhow::anyhow!("mission inventory quantity overflow")))?;
    Ok(available >= i128::from(qty))
}

/// Plan delivered goods before acquiring item locks.
async fn plan_goods(db: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, me: i64, station: i64, ship: i64, (type_id, qty): (i32, i32)) -> Result<(Vec<crate::dao::items::loading::Item>, std::collections::BTreeMap<i64, crate::dao::items::quantities::QuantityChange>), CallError> {
    use crate::dao::items::{loading::ItemQuery, quantities::QuantityChange};
    let items = crate::services::items::load_items(db, sde, ItemQuery::OwnerTypeTwoLocations(me, type_id, station, FLAG_HANGAR, ship, FLAG_CARGO)).await.map_err(db_err)?;
    if qty <= 0 { return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR)); }
    let mut left = i64::from(qty);
    let mut changes = std::collections::BTreeMap::new();
    for item in &items {
        if left <= 0 { break; }
        let available = crate::services::item_volume::stack_size(item.singleton, item.quantity);
        let take = available.min(left).max(0);
        if take == 0 { continue; }
        left -= take;
        let remaining = if item.singleton || take == available { None } else { Some(item.quantity - take) };
        changes.insert(item.item_id, QuantityChange::new(item, remaining));
    }
    if left > 0 { return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR)); }
    Ok((items, changes))
}

/// Apply the guarded plan, retaining inventory notification order.
async fn take_goods<X: Send>(db: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut crate::db::outbox::Outbox<X>, items: Vec<crate::dao::items::loading::Item>, changes: std::collections::BTreeMap<i64, crate::dao::items::quantities::QuantityChange>) -> Result<(), CallError> {
    if !crate::dao::items::quantities::apply(db, &changes).await.map_err(db_err)? {
        return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
    }
    for item in items {
        if let Some(change) = changes.get(&item.item_id) {
            if change.quantity.is_none() {
                super::inventory::notifications::notify_item_removed(outbox, item);
            } else {
                super::inventory::notifications::notify_item_change(db, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity))]).await?;
            }
        }
    }
    Ok(())
}

/// Can mission `m` be handed in to `agent` now?
pub(super) async fn ready(c: &Client, m: &Mission, agent: &Agent) -> Result<bool, CallError> {
    if m.state != STATE_ACCEPTED || m.hand_in_agent() != agent.id {
        return Ok(false);
    }
    Ok(match m.kind {
        Kind::Kill => objective_done(c, m).await,
        Kind::Talk => true,
        Kind::Courier | Kind::Trade => match m.cargo {
            Some(cargo) => has_goods(c, agent.station_id, cargo).await?,
            None => true,
        },
        Kind::Interaction => false,
    })
}

/// Close a mission: take delivered goods, pay its rewards (ISK, LP-less items into the
/// hangar at `agent`'s station) and, for an arc step, unlock what follows (`choice`: the
/// branch an agent interaction picked).
pub(super) async fn finish(c: &mut Client, m: &Mission, agent: &Agent, choice: Option<usize>) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let ship = if c.session.int("stationid") == Some(agent.station_id) { c.session.int("shipid").unwrap_or(0) } else { 0 };
    let fact_ship = c.session.int("shipid").filter(|id| *id > 0);
    let fact_system = c.session.int("solarsystemid2").filter(|id| *id > 0);
    let fact_station = c.session.int("stationid").filter(|id| *id > 0);
    let cargo = if matches!(m.kind, Kind::Courier | Kind::Trade) { m.cargo } else { None };
    let mut reward = None;
    let mut unlocked = Vec::new();
    if m.arc_id.is_some() {
        if let Some(data) = c.sde.missions.get(m.mission_id) {
            if let (Some(type_id), Some(quantity)) = (data.reward_type.filter(|id| *id != TYPE_CREDITS as i32), data.reward_quantity) {
                reward = Some((type_id, quantity.max(1)));
            }
        }
        let next = c.sde.missions.step(m.mission_id).map(|s| s.next.clone()).unwrap_or_default();
        unlocked = match choice {
            Some(k) => next.get(k).or(next.last()).copied().into_iter().collect(),
            None => next,
        };
    }
    let mission_id = m.mission_id;
    let mission_agent = m.agent_id;
    let arc = m.arc_id;
    let offered_interaction = m.state == STATE_OFFERED && m.kind == Kind::Interaction;
    let reward_isk = m.reward_isk;
    let reward_lp = m.reward_lp;
    let name_id = (m.name_id != 0).then_some(m.name_id);
    let (agent_id, corp, faction, level, station) = (agent.id, agent.corp_id, agent.faction_id, agent.level, agent.station_id);
    let stamp = filetime_now();
    // The positive standing gain is quoted once from the pilot's current skills and
    // boosters, before the market lock; only the scalar crosses into the transaction.
    let gain_multiplier = standings::mutation::mission_gain_multiplier(&Fit::load_character_at(&c.db, c.sde.clone(), me, stamp).await?);
    let arc_states = arc.map(|_| {
        let mut states = vec![(mission_id, "completed", Some(stamp))];
        states.extend(unlocked.into_iter().map(|id| (id, "unlocked", None)));
        states.sort_by_key(|row| row.0);
        std::sync::Arc::new(states)
    });
    let _lock = super::super::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut crate::db::outbox::Outbox<()>| {
            let arc_states = arc_states.clone();
            let sde = sde.clone();
            Box::pin(async move {
                standings::mutation::mission_completed(tx, outbox, me, agent_id, corp, faction, level, gain_multiplier, name_id, stamp).await?;
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                let goods = if let Some(cargo) = cargo {
                    Some(plan_goods(tx, &sde, me, station, ship, cargo).await?)
                } else { None };
                let mut reward_plan = if arc.is_some() {
                    if let Some((type_id, qty)) = reward {
                        let destination = crate::services::inventory::destination::Destination::owned(me, station, FLAG_HANGAR);
                        Some((super::items::DepositPreparation::capture(tx, &sde, &destination, type_id).await?, qty))
                    } else { None }
                } else { None };
                let source_hull = goods.as_ref().is_some_and(|(items, changes)| items.iter().any(|item| item.location_id == ship && changes.contains_key(&item.item_id)));
                let locks = if source_hull || reward_plan.as_ref().is_some_and(|(prep, _)| !prep.is_excluded()) {
                    let mut requests: Vec<_> = goods.as_ref().into_iter().flat_map(|(_, changes)| changes.keys())
                        .map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
                    if source_hull {
                        requests.push(LockRequest { item_id: ship, strength: LockStrength::Update });
                    }
                    let mut candidates = Vec::new();
                    if let Some((preparation, _)) = &reward_plan {
                        if let Some(request) = preparation.candidate_request() {
                            if !requests.iter().any(|mandatory| mandatory.item_id == request.item_id) {
                                candidates.push(request.item_id);
                            }
                            requests.push(request);
                        }
                    }
                    Some(crate::services::inventory::destination::lock_items(tx, &sde, &requests, &candidates).await?)
                } else {
                    None
                };
                if let Some((items, changes)) = goods {
                    take_goods(tx, &sde, outbox, items, changes).await?;
                }
                if offered_interaction && crate::dao::missions::accept_offered(tx, me, mission_agent, mission_id, STATE_OFFERED, STATE_ACCEPTED, stamp).await.map_err(db_err)? == 0 {
                    return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                }
                if offered_interaction {
                    outbox.event(crate::lua::events::GameEvent::MissionAccepted { char_id: me, agent_id: mission_agent, mission_id });
                }
                if crate::dao::missions::remove_observed(tx, me, mission_agent, mission_id, STATE_ACCEPTED).await.map_err(db_err)? == 0 {
                    return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                }
                outbox.operation(());
                let gross = crate::isk::IskAmount::from_whole_isk(i128::from(reward_isk))
                    .ok_or(crate::isk::AmountError)?;
                crate::services::corps::tax::settle_reward(tx, outbox, me, gross, Some(crate::services::corps::tax::LoyaltyReward {
                    issuer: corp, amount: reward_lp,
                    mutation: super::standings::LoyaltyMutation {
                        reason: crate::lua::events::resources::LoyaltyReason::MissionReward,
                        occurred_at: filetime_now(),
                        context: crate::lua::events::resources::FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: fact_ship, ship_type_id: None, system_id: fact_system, station_id: fact_station, origin: crate::lua::events::resources::FactOrigin::Natural },
                    },
                })).await?;
                if let Some(arc) = arc {
                    if let Some((preparation, qty)) = &mut reward_plan {
                        super::items::deposit_prepared(tx, &sde, outbox, preparation, *qty, 0, locks.as_ref(), None, Default::default(), crate::services::inventory::hold_storage::CapacitySource::Current).await?;
                    }
                    for &(id, state, state_stamp) in arc_states.as_ref().unwrap().iter() {
                        if offered_interaction && id == mission_id && state == "completed" {
                            crate::dao::missions::set_arc_state(tx, me, arc, id, "accepted", Some(stamp)).await.map_err(db_err)?;
                        }
                        crate::dao::missions::set_arc_state(tx, me, arc, id, state, state_stamp).await.map_err(db_err)?;
                    }
                }
                outbox.event(crate::lua::events::GameEvent::MissionCompleted { char_id: me, agent_id: mission_agent, mission_id });
                outbox.notify_client("OnAgentMissionChange", vec![Value::str("completed"), Value::Int(mission_agent)]);
                Ok(())
            })
        },
        |(), outbox| dispatch_mission_outbox(c, me, mission_agent, outbox),
    ).await?;
    info!(user = c.login.user_id, "agent {}: mission {} completed at {}, {} ISK", mission_agent, mission_id, agent_id, reward_isk);
    Ok(())
}

/// An agent interaction's answers: messages.mission.option<N>.charsays.
pub(super) async fn options(c: &Client, mission_id: i32) -> Result<Vec<i64>, CallError> {
    let mut out = vec![];
    for n in 1..=4 {
        match message(c, mission_id, &format!("messages.mission.option{n}.charsays")).await? {
            Some(id) => out.push(id),
            None => break,
        }
    }
    Ok(out)
}


pub(super) async fn remove(c: &Client, agent_id: i64) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    crate::dao::missions::remove(&c.db, me, agent_id).await.map_err(db_err)?;
    despawn(c, me, agent_id);
    Ok(())
}

/// Dispatch a committed mission transaction's outbox: notifications go to the
/// world/client and each `()` operation despawns the mission's site actor.
/// Shared by `finish` and `end_mission` so the post-commit path stays single.
async fn dispatch_mission_outbox(c: &mut Client, me: i64, agent_id: i64, outbox: crate::db::outbox::Outbox<()>) -> Result<(), CallError> {
    let world = c.world.clone();
    for entry in outbox.into_entries() {
        match entry {
            crate::db::outbox::Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *c))?,
            crate::db::outbox::Effect::Operation(()) => despawn(c, me, agent_id),
        }
    }
    Ok(())
}

/// End a mission without paying it: decline an offer or quit an accepted one.
/// The standing loss, the guarded removal of the observed mission row, a quit
/// step's arc updates, the despawn operation and `OnAgentMissionChange` are one
/// transaction; a zero-row removal refuses the whole batch so standing is never
/// charged for a mission that is already gone. Despawn and notifications run
/// only after commit. `quit == true` journals a failed mission, not a declined
/// one; a quit arc step's retry (usually itself) is unlocked again.
pub(super) async fn end_mission(c: &mut Client, mission: &Mission, agent: &Agent, quit: bool) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let (agent_id, corp) = (agent.id, agent.corp_id);
    let (mission_id, state) = (mission.mission_id, mission.state);
    let name_id = (mission.name_id != 0).then_some(mission.name_id);
    let stamp = filetime_now();
    // A quit arc step can be taken up again (its fail mission, usually itself);
    // declining an offer never touches arc progress.
    let arc = if quit { mission.arc_id.map(|arc| (arc, c.sde.missions.step(mission_id).and_then(|s| s.fail).unwrap_or(mission_id))) } else { None };
    let db = c.db.clone();
    db.transaction(
        move |tx, outbox: &mut crate::db::outbox::Outbox<()>| {
            Box::pin(async move {
                standings::mutation::mission_declined(tx, outbox, me, agent_id, corp, name_id, quit, stamp).await?;
                if crate::dao::missions::remove_observed(tx, me, agent_id, mission_id, state).await.map_err(db_err)? == 0 {
                    return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                }
                if let Some((arc, retry)) = arc {
                    // Ascending mission_id order; equal ids keep quit-then-unlocked.
                    let mut writes = [(mission_id, "quit", Some(stamp)), (retry, "unlocked", None)];
                    writes.sort_by_key(|write| write.0);
                    for (id, state, state_stamp) in writes {
                        crate::dao::missions::set_arc_state(tx, me, arc, id, state, state_stamp).await.map_err(db_err)?;
                    }
                }
                outbox.operation(());
                outbox.notify_client("OnAgentMissionChange", vec![Value::str(if quit { "quit" } else { "offer_declined" }), Value::Int(agent_id)]);
                Ok(())
            })
        },
        |(), outbox| dispatch_mission_outbox(c, me, agent_id, outbox),
    ).await
}


#[cfg(all(test, feature = "tests-content"))] mod tests;
