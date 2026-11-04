//! Mission state, offers, rewards and arc mechanics; completion commits before dispatching
//! effects. The client renders dungeon objectives in
//! `agentinteraction/objectivesteps.py:103-104,123-125,137-142`.
//! Mission and dungeon candidates, messages and placement use the shared SDE.
//! Mission, character, item, loyalty and standing state is
//! stored in the game database; site simulation is separate.
//! Basic mission rewards and placement are operator policy, not a client-provided generator;
//! complete live-server mission layouts and mechanics are not reproduced.
//! Committed mission completions and reward wallet changes queue Lua events.
use super::*;
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
pub(super) async fn offer(c: &Client, agent: &Agent) -> Result<(), CallError> {
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
    let (isk, lp) = rewards(agent.level);
    crate::dao::missions::offer(&c.db, &crate::dao::missions::Offer {
        character_id: me, agent_id: agent.id, mission_id: pick.mission_id, dungeon_id,
        faction_id, state: STATE_OFFERED, system_id: agent.system_id, pos,
        reward_isk: isk, reward_lp: lp, offered_at: filetime_now(),
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

pub(super) async fn set_arc_state(c: &Client, arc: i64, mission: i32, state: &str) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let stamp = if state == "unlocked" { None } else { Some(filetime_now()) };
    crate::dao::missions::set_arc_state(&c.db, me, arc, mission, state, stamp).await.map_err(db_err)?;
    Ok(())
}

/// Offer an epic arc step: its content (kill site, courier cargo, trade goods...) and
/// rewards come from the mission's own data.
pub(super) async fn offer_arc(c: &Client, agent: &Agent, mission_id: i32) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(step) = c.sde.missions.step(mission_id) else { return Ok(()) };
    let r = c.sde.missions.get(mission_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing mission {mission_id}")))?;
    let dungeon_id = r.dungeon_id.unwrap_or(0);
    let faction_id = r.dungeon_id.and_then(|id| c.sde.dungeons.get(i64::from(id))).and_then(|d| d.faction_id).or(r.faction_id).unwrap_or(0);
    let reward_isk = if r.reward_type == Some(TYPE_CREDITS as i32) { r.reward_quantity.expect("credit reward quantity is NULL") } else { 0 };
    let reward_lp = r.reward_lp.unwrap_or(0);
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
            reward_isk, reward_lp, offered_at: filetime_now(),
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
    Ok(goods_at(c, station, type_id).await?.iter().map(|i| crate::services::item_volume::stack_size(i.singleton, i64::from(i.quantity))).sum::<i64>() >= qty as i64)
}

/// Take delivered goods using guarded item changes, retaining inventory notification order.
async fn take_goods<X: Send>(db: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut crate::db::outbox::Outbox<X>, me: i64, station: i64, ship: i64, (type_id, qty): (i32, i32)) -> Result<(), CallError> {
    use crate::dao::items::{loading::ItemQuery, quantities::{self, QuantityChange}};
    let items = crate::services::items::load_items(db, sde, ItemQuery::OwnerTypeTwoLocations(me, type_id, station, FLAG_HANGAR, ship, FLAG_CARGO)).await.map_err(db_err)?;
    if qty <= 0 { return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR)); }
    let mut left = qty;
    let mut changes = std::collections::BTreeMap::new();
    for item in &items {
        if left <= 0 { break; }
        let available = crate::services::item_volume::stack_size(item.singleton, i64::from(item.quantity));
        let take = available.min(i64::from(left)).max(0) as i32;
        if take == 0 { continue; }
        left -= take;
        let remaining = if item.singleton || i64::from(take) == available { None } else { Some(item.quantity - take) };
        changes.insert(item.item_id, QuantityChange::new(item, remaining));
    }
    if left > 0 || !quantities::apply(db, &changes).await.map_err(db_err)? {
        return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
    }
    for item in items {
        if let Some(change) = changes.get(&item.item_id) {
            if change.quantity.is_none() {
                super::inventory::notifications::notify_item_removed(outbox, item);
            } else {
                super::inventory::notifications::notify_item_change(db, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?;
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
    let cargo = if matches!(m.kind, Kind::Courier | Kind::Trade) { m.cargo } else { None };
    let mut reward = None;
    let mut unlocked = Vec::new();
    if m.arc_id.is_some() {
        if let Some(data) = c.sde.missions.get(m.mission_id) {
            if let (Some(type_id), Some(quantity)) = (data.reward_type.filter(|id| *id != TYPE_CREDITS as i32), data.reward_quantity) {
                let qty = i32::try_from(quantity).map_err(|_| CallError::Internal(anyhow::anyhow!("mission reward quantity out of range")))?;
                reward = Some((type_id, qty.max(1)));
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
    let (agent_id, corp, faction, level, station) = (agent.id, agent.corp_id, agent.faction_id, agent.level, agent.station_id);
    let stamp = filetime_now();
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
                if let Some(cargo) = cargo {
                    take_goods(tx, &sde, outbox, me, station, ship, cargo).await?;
                }
                if offered_interaction && crate::dao::missions::accept_offered(tx, me, mission_agent, mission_id, STATE_OFFERED, STATE_ACCEPTED, stamp).await.map_err(db_err)? == 0 {
                    return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                }
                if offered_interaction {
                    outbox.event(crate::lua::events::GameEvent::MissionAccepted { char_id: me, agent_id: mission_agent, mission_id });
                }
                if crate::dao::missions::remove_accepted(tx, me, mission_agent, mission_id, STATE_ACCEPTED).await.map_err(db_err)? == 0 {
                    return Err(super::super::notify_error(messages::MISSION_DELIVER_TO_STATION_HANGAR));
                }
                outbox.operation(());
                if reward_isk > 0 {
                    let balance = crate::dao::wallets::characters::credit(tx, me, reward_isk as f64).await.map_err(db_err)?;
                    outbox.notify_client("OnAccountChange", vec![Value::str("cash"), Value::Int(me), Value::Float(balance)]);
                    outbox.event(crate::lua::events::GameEvent::WalletChanged { owner_id: me, account_key: crate::consts::ACCOUNT_CASH, balance, delta: reward_isk as f64 });
                }
                super::standings::add_lp(tx, outbox, me, corp, reward_lp).await?;
                super::standings::mission_completed(tx, outbox, me, agent_id, corp, faction, level).await?;
                if let Some(arc) = arc {
                    if let Some((type_id, qty)) = reward {
                        // Deposit selects its target stack itself, which may sort below consumed
                        // stacks. The economy lock serializes economy operations; the transaction
                        // runner retries deadlocks once.
                        super::items::deposit(tx, &sde, outbox, me, station, FLAG_HANGAR, type_id, qty, 0).await?;
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
        |(), outbox| {
            let client = &mut *c;
            async move {
                let world = client.world.clone();
                for entry in outbox.into_entries() {
                    match entry {
                        crate::db::outbox::Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *client))?,
                        crate::db::outbox::Effect::Event(event) => crate::lua::on_event(event),
                        crate::db::outbox::Effect::Operation(()) => despawn(client, me, mission_agent),
                    }
                }
                Ok(())
            }
        },
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


#[cfg(test)] mod tests;
