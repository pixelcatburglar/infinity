//! Pilot preparation, ordered departure, and process-local pilot and chat presence.
//! `carbon/common/script/net/machoNet.py:3954-3957` binds the new charid to the
//! client and releases the old one in `ProcessSessionChange`. Preparation and
//! departure use character, item and skill data and the SDE to restore or save
//! pilots. Interactive departure checkpoints the actor's selected pilot even when
//! a committed boarding receipt was not applied to the departing client session.
//! The process-local pilot registry also supplies station guest records:
//! online, station changes and offline send arrival or departure notices to
//! other registered guests in the same station. Transport lifecycle and
//! cross-process presence are not managed here.

use std::sync::{Arc, Mutex, Weak};

use crate::chat::Hub;
use crate::dao::{characters, items, skills};
use crate::dao::characters::selection::SelectedCharacter;
use crate::db::{Db, outbox::Notification};
use crate::sde::Sde;
use infinity_core::session::OnlineSession;
use crate::space::{NotifyTx, World};
use super::identity::Identity;
use super::{db_err, CallError};

pub(crate) fn guest_record(session: &OnlineSession) -> marshal::Value {
    marshal::Value::tuple([
        marshal::Value::Int(session.character_id),
        session.corporation_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None),
        session.alliance_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None),
        session.faction_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None),
    ])
}

fn station_notice(world: &World, station: i64, session: &OnlineSession, event: &str) {
    for guest in world.station_guests(station) {
        if guest.character_id != session.character_id {
            world.notify_pilot(guest.character_id, event, vec![guest_record(session)]);
        }
    }
}

pub(crate) struct Prepared {
    pub ship_id: i64,
    pub cloak_until: Option<i64>,
}

async fn publish_selected_ship(
    db: &Db, world: &World, sde: &Arc<Sde>, char_id: i64, ship: i64,
    selected: &SelectedCharacter,
) -> Result<i64, CallError> {
    // Reconciliation owns its own market guard and finishes before this one.
    // Lock order: market, database reads, then actor publication.
    let _lock = super::market::lock().await;
    if !crate::dao::combat::victim_current(db, char_id, ship, selected.solar_system_id).await.map_err(db_err)? {
        return Err(CallError::Internal(anyhow::anyhow!("selected ship is no longer in space")));
    }
    let s = characters::selection::space_ship(db, char_id).await.map_err(db_err)?
        .filter(|s| s.item_id == ship)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected ship is no longer in space")))?;
    let (Some(x), Some(y), Some(z)) = (s.x, s.y, s.z) else {
        return Err(CallError::Internal(anyhow::anyhow!("selected ship has no space position")));
    };
    let type_id = items::inventory::type_id(db, ship).await.map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing SDE ship item")))?;
    let vessel = items::space_ships::FreeShip {
        item_id: ship, type_id, owner_id: char_id, corporation_id: selected.corporation_id,
        alliance_id: selected.alliance_id.unwrap_or(-1), pos: [x, y, z],
    };
    let security = super::crimewatch::security_status(db, char_id).await?;
    let ball = super::ship::balls::piloted(db, sde.clone(), &vessel, char_id, security).await?;
    super::ship::arrive_cloaked(world, selected.solar_system_id, ball, &_lock).await
}

pub(crate) async fn prepare(
    db: &Db, world: &World, sde: &Arc<Sde>, char_id: i64,
    selected: &SelectedCharacter,
    mut notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> Result<Prepared, CallError> {
    super::skills::expert_systems::advance_expired(db, world, sde, char_id, crate::client::filetime_now(), &mut notify).await?;
    let station = selected.station_id;
    let race = selected.race_id;
    let space_ship = characters::selection::space_ship(db, char_id).await.map_err(db_err)?;
    let ship = match (&space_ship, station) {
        (Some(s), None) => s.item_id,
        (_, Some(st)) => super::character::provisioning::ensure_ship(db, sde, char_id, race, st).await?,
        (None, None) => return Err(CallError::Internal(anyhow::anyhow!("character {char_id} is in space without a ship"))),
    };
    let has_skills = skills::selection::has_skills(db, char_id).await.map_err(db_err)?;
    if !has_skills {
        super::skills::starting::grant_starting_skills(db, sde, char_id, race).await?;
    }
    super::skill_plans::recovery::character(db, sde, char_id).await?;
    super::effects::online::reconcile_ship(db, world, sde, char_id, ship, notify).await?;
    if station.is_some() {
        super::combat::load_docked_condition(db, world, char_id, ship).await?;
    }
    let cloak_until = if station.is_none() {
        Some(publish_selected_ship(db, world, sde, char_id, ship, selected).await?)
    } else {
        None
    };
    Ok(Prepared { ship_id: ship, cloak_until })
}

pub(crate) fn online(identity: &Identity, world: &World, chat: &Hub, notify_tx: NotifyTx, previous: Option<&OnlineSession>, current: OnlineSession) {
    let previous = previous.filter(|s| s.character_id == current.character_id);
    let old_station = previous.and_then(|s| s.station_id);
    let registered = world.online_session(current.character_id);
    if let (Some(old), Some(live)) = (previous, registered.as_ref()) {
        if old.session_id != live.session_id { return; }
    }
    let same_generation = previous.is_some_and(|s| s.session_id == current.session_id);
    let only_ship_refresh = same_generation && previous.is_some_and(|s| {
        let mut old = s.clone();
        old.ship_id = current.ship_id;
        old == current
    });
    if previous.map(|s| s.character_id) != Some(identity.char_id) {
        world.register_pilot(identity.char_id, notify_tx);
    }
    world.set_online_session(identity.char_id, Some(current.clone()));
    if only_ship_refresh { return; }
    chat.session_changed(identity.char_id);
    if let Some(old) = previous {
        if old.corporation_id != current.corporation_id || old.alliance_id != current.alliance_id
            || old.faction_id != current.faction_id || old.role != current.role {
            chat.refresh_profile(&current);
            let old_channels = super::chat::authority::system_channels(world.sde(), old);
            for channel in super::chat::authority::system_channels(world.sde(), &current) {
                if (channel.starts_with("corp_") || channel.starts_with("alliance_")) && !old_channels.contains(&channel) {
                    chat.invite(current.character_id, &channel);
                }
            }
        }
        if (old.corporation_id != current.corporation_id || old.alliance_id != current.alliance_id)
            && current.station_id.is_none() {
            if let (Some(system), Some(ship)) = (current.system_id, current.ship_id) {
                let character = current.character_id;
                let corp = current.corporation_id;
                let alliance = current.alliance_id;
                world.post(system, move |s| s.refresh_pilot_affiliation(character, ship, corp, alliance));
            }
        }
    }
    if old_station != current.station_id {
        if let (Some(station), Some(old)) = (old_station, previous) {
            station_notice(world, station, old, "OnCharNoLongerInStation");
        }
        if let Some(station) = current.station_id {
            station_notice(world, station, &current, "OnCharNowInStation");
        }
    }
}


pub(crate) fn offline_generation(identity: &Identity, world: &World, chat: &Hub, session_id: Option<i64>) -> bool {
    let Some(previous) = world.online_session(identity.char_id) else { return false };
    let generation = session_id.unwrap_or(previous.session_id);
    if previous.session_id != generation || !world.unregister_pilot_generation(identity.char_id, Some(generation)) { return false; }
    chat.disconnect_character(identity.char_id);
    if let Some(station) = previous.station_id {
        station_notice(world, station, &previous, "OnCharNoLongerInStation");
    }
    true
}

type LogoutResult = Result<(), String>;
type ConditionRows = [Option<(f64, i64)>; 3];
enum LogoutAdmission { NoMembership, Unavailable, Submitted }
#[derive(Clone, Copy)]
enum LogoutCheckpoint { Committed, Unchanged, Moved }

struct LogoutCleanup {
    identity: Identity,
    db: Db,
    world: Weak<World>,
    chat: Weak<Hub>,
    sde: Arc<Sde>,
    expected: Option<OnlineSession>,
    unobserve: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    receipt: Mutex<Option<tokio::sync::oneshot::Sender<LogoutResult>>>,
}

impl LogoutCleanup {
    async fn finish_without_checkpoint(&self, user_id: Option<i64>) {
        let Some(world) = self.world.upgrade() else {
            self.finish(Err("world stopped during logout".into())).await;
            return;
        };
        let character = self.identity.char_id;
        if let Some(user_id) = user_id.filter(|_| character != 0 && self.expected.is_some() && world.online_session(character) == self.expected) {
            let result = self.db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(async move {
                    characters::selection::logoff(tx, character).await.map_err(db_err)?;
                    outbox.event(crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id: character });
                    Ok::<_, CallError>(())
                }),
                |(), outbox| {
                    let world = &world;
                    async move {
                    self.finish(Ok(())).await;
                    crate::db::outbox::dispatch_world((), outbox, world)
                    }
                },
            ).await;
            if let Err(error) = result { self.finish(Err(format!("{error:?}"))).await; }
        } else {
            self.finish(Ok(())).await;
        }
    }

    async fn finish(&self, mut result: LogoutResult) {
        if let Some(unobserve) = self.unobserve.lock().unwrap().take() { unobserve(); }
        if let Some(world) = self.world.upgrade() {
            let character = self.identity.char_id;
            let current = world.online_session(character);
            let matching = self.expected.is_some() && current == self.expected;
            if character != 0 && matching {
                let checkpoint_ok = result.is_ok();
                let removed = if let Some(chat) = self.chat.upgrade() {
                    offline_generation(&self.identity, &world, &chat, self.expected.as_ref().map(|session| session.session_id))
                } else {
                    let removed = world.unregister_pilot_generation(character, self.expected.as_ref().map(|session| session.session_id));
                    if result.is_ok() { result = Err("chat stopped during logout cleanup".into()); }
                    removed
                };
                if removed {
                    super::drones::settings::logged_off(character);
                    if checkpoint_ok {
                        if let Err(error) = super::corps::pending_kicks::trigger(&self.db, &world, character).await {
                            if result.is_ok() { result = Err(format!("{error:?}")); }
                        }
                    }
                    super::online_status::presence_changed(&self.db, &world, character, false).await;
                    if let Err(error) = super::trade::on_leave(&self.identity, &self.db, &world, &self.sde).await {
                        let message = match error {
                            CallError::Internal(error) => error.to_string(),
                            CallError::User { msg, .. } => msg.to_string(),
                        };
                        tracing::warn!(%message, "trade cleanup on leave failed");
                        if result.is_ok() { result = Err(message); }
                    }
                }
            }
        } else if result.is_ok() {
            result = Err("world stopped during logout".into());
        }
        if let Some(sender) = self.receipt.lock().unwrap().take() { let _ = sender.send(result); }
    }
}

struct LogoutSettlement {
    cleanup: Arc<LogoutCleanup>,
    system: i64,
    ship: i64,
    token: u64,
    snapshot: crate::space::combat::ActorConditionSnapshot,
    facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>,
    position: [f64; 3],
    before: Mutex<Option<(Option<[f64; 3]>, ConditionRows)>>,
    logoff_possible: bool,
    skipped_logoff: std::sync::atomic::AtomicBool,
    before_logoff: Mutex<Option<(String, Option<String>)>>,
}

impl LogoutSettlement {
    fn logoff_user(&self, user_id: Option<i64>, world: &World) -> Option<i64> {
        let user_id = user_id.filter(|_| self.cleanup.expected.is_some()
            && world.online_session(self.cleanup.identity.char_id) == self.cleanup.expected);
        if user_id.is_none() { self.skipped_logoff.store(true, std::sync::atomic::Ordering::SeqCst); }
        user_id
    }

    fn callback(self: &Arc<Self>) -> Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> {
        let plan = self.clone();
        Arc::new(move || {
            let plan = plan.clone();
            Box::pin(async move { plan.reconcile().await })
        })
    }

    async fn read_checkpoint(&self, ex: &impl crate::db::executor::Executor) -> Result<(bool, Option<[f64; 3]>, ConditionRows), crate::db::Error> {
        use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_SHIELD_CHARGE};
        let current = crate::dao::combat::victim_current(ex, self.cleanup.identity.char_id, self.ship, self.system).await?;
        if !current { return Ok((false, None, [None; 3])); }
        let position = items::social::position(ex, self.cleanup.identity.char_id).await?
            .filter(|p| p.location_id == self.system).map(|p| [p.x, p.y, p.z]);
        let rows = items::dogma::overrides(ex, self.ship).await?;
        let values = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE].map(|id|
            rows.iter().find(|r| r.attribute_id == id).map(|r| (r.value, r.updated_at)));
        Ok((true, position, values))
    }

    async fn retire(&self, world: &World) -> Option<bool> {
        let (ship, token, generation) = (self.ship, self.token, self.snapshot.generation);
        world.with(self.system, move |s| {
            if s.combat_membership_generation(ship) != Some(generation)
                || s.combat_phase(ship) != Some(crate::space::combat::CombatPhase::Transition { token }) {
                return false;
            }
            s.commit_combat_transition(ship, token) && (!s.contains(ship) || s.remove_ball(ship).is_some())
        }).await
    }

    async fn reconcile(self: Arc<Self>) {
        let Some(world) = self.cleanup.world.upgrade() else {
            self.cleanup.finish(Err("world stopped during logout reconciliation".into())).await;
            return;
        };
        let market = super::market::lock().await;
        let gate = match world.lock_condition_snapshot(self.system).await {
            Ok(gate) => gate,
            Err(_) => {
                drop(market);
                self.cleanup.finish(Err("system stopped during logout reconciliation".into())).await;
                return;
            }
        };
        let read = self.cleanup.db.transaction(
            |tx, _: &mut ()| {
                let plan = self.clone();
                Box::pin(async move {
                    characters::selection::lock_character(tx, plan.cleanup.identity.char_id).await?;
                    items::ship::lock_reload_ship(tx, plan.ship).await?;
                    let read = plan.read_checkpoint(tx).await?;
                    let (committed, verified) = match &plan.facts {
                        Some(facts) => {
                            let committed = facts.is_committed(tx).await?;
                            let verified = if committed { facts.verified_condition_ids(tx).await? } else { Default::default() };
                            (Some(committed), verified)
                        }
                        None => (None, Default::default()),
                    };
                    let timestamps = if plan.logoff_possible {
                        Some(characters::selection::logout_timestamps(tx, plan.cleanup.identity.char_id).await?)
                    } else { None };
                    Ok::<_, crate::db::Error>((read, committed, verified, timestamps))
                })
            },
            |value, ()| async move { Ok(value) },
        ).await;
        let before = *self.before.lock().unwrap();
        let prior_logoff = self.before_logoff.lock().unwrap().clone();
        let mut verified = Default::default();
        let outcome = match (read, before) {
            (Ok(((false, _, _), _, _, _)), _) => Some(LogoutCheckpoint::Moved),
            (Ok(((true, position, rows), fact_receipt, written, timestamps)), Some((prior_pos, prior_rows))) => {
                verified = written;
                let condition = &self.snapshot.condition;
                let expected = [
                    (condition.now[0], condition.shield_at),
                    (condition.stats.max[1] - condition.now[1], 0),
                    (condition.stats.max[2] - condition.now[2], 0),
                ];
                let hp_matches = self.snapshot.revision == self.snapshot.saved_revision
                    || rows == expected.map(Some);
                let unchanged = position == prior_pos && rows == prior_rows;
                let no_event = !self.logoff_possible || self.skipped_logoff.load(std::sync::atomic::Ordering::SeqCst);
                let business_proven = if !no_event {
                    timestamps.is_some() && prior_logoff.is_some() && timestamps != prior_logoff
                } else { position == Some(self.position) && hp_matches };
                if business_proven && fact_receipt != Some(false) {
                    Some(LogoutCheckpoint::Committed)
                } else if unchanged && (fact_receipt == Some(false) || (no_event && fact_receipt.is_none()))
                    && (no_event || timestamps == prior_logoff) {
                    Some(LogoutCheckpoint::Unchanged)
                } else if !business_proven || fact_receipt == Some(false) {
                    None
                } else { Some(LogoutCheckpoint::Moved) }
            }
            _ => None,
        };
        if let Some(outcome) = outcome {
            let result = if matches!(outcome, LogoutCheckpoint::Committed) {
                if let Some(facts) = self.facts.clone() {
                    let _ = world.with(self.system, move |s| facts.acknowledge(s, &verified)).await;
                }
                match self.retire(&world).await {
                    Some(true) => {
                        let character = self.cleanup.identity.char_id;
                        if world.online_session(character) == self.cleanup.expected {
                            super::combat::reset_pilot(&world, character, Some((self.system, self.ship)));
                        }
                        Ok(())
                    }
                    Some(false) => Err("logout membership changed during settlement".into()),
                    None => Err("system stopped during logout settlement".into()),
                }
            } else if matches!(outcome, LogoutCheckpoint::Moved) {
                let _ = self.retire(&world).await;
                Err("ship moved during logout settlement".into())
            } else {
                let (ship, token, generation) = (self.ship, self.token, self.snapshot.generation);
                match world.with(self.system, move |s| s.cancel_combat_transition(ship, token, generation)).await {
                    Some(true) => Err("logout checkpoint did not commit".into()),
                    Some(false) => Err("logout membership changed during reconciliation".into()),
                    None => Err("system stopped during logout reconciliation".into()),
                }
            };
            drop(gate);
            drop(market);
            self.cleanup.finish(result).await;
        } else {
            let (ship, token, generation) = (self.ship, self.token, self.snapshot.generation);
            let released = world.with(self.system, move |s| s.release_pending_combat(ship, token, generation)).await;
            drop(gate);
            drop(market);
            if released != Some(true) {
                self.cleanup.finish(Err("system stopped during logout reconciliation".into())).await;
            }
        }
    }
}

/// Logout checkpoints the matching pilot before removal, independently of the caller.
pub(crate) async fn leave(
    identity: &Identity, user_id: Option<i64>, db: &Db, world: &Arc<World>, chat: &Arc<Hub>, sde: &Arc<Sde>,
    space_system: Option<i64>, session_id: Option<i64>, unobserve: impl FnOnce() + Send + 'static,
) -> anyhow::Result<()> {
    let expected = world.online_session(identity.char_id);
    if let Some(sid) = session_id {
        if expected.as_ref().is_some_and(|live| live.session_id != sid || live.user_id != user_id) {
            unobserve();
            return Ok(());
        }
    }
    let char_id = identity.char_id;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let cleanup = Arc::new(LogoutCleanup {
        identity: *identity, db: db.clone(), world: Arc::downgrade(world), chat: Arc::downgrade(chat),
        sde: sde.clone(), expected, unobserve: Mutex::new(Some(Box::new(unobserve))),
        receipt: Mutex::new(Some(sender)),
    });
    if let (Some(ship), Some(system)) = (identity.ship_id, space_system.filter(|_| char_id != 0)) {
        let market = super::market::lock().await;
        let gate = match world.lock_condition_snapshot(system).await {
            Ok(gate) => gate,
            Err(error) => {
                drop(market);
                cleanup.finish(Err(error.to_string())).await;
                return receiver.await.map_err(|_| anyhow::anyhow!("logout worker unavailable"))?.map_err(anyhow::Error::msg);
            }
        };
        let owned = cleanup.clone();
        let submitted = world.with(system, move |s| {
            if !s.combat_session_current(char_id, owned.expected.as_ref()) { return LogoutAdmission::NoMembership; }
            let actual_pilot = session_id.is_some_and(|sid| owned.expected.as_ref().is_some_and(|live|
                live.session_id == sid && live.user_id == user_id && user_id.is_some()
                    && live.character_id == char_id && live.system_id == Some(system)
                    && live.ship_id == Some(ship)));
            let Some(ball) = s.pilot_ball(char_id).filter(|ball| actual_pilot || ball.id == ship).cloned() else { return LogoutAdmission::NoMembership };
            let ship = ball.id;
            let Some(transition) = s.begin_condition_transitions(&[ship]) else { return LogoutAdmission::Unavailable };
            let logoff_possible = user_id.is_some() && owned.expected.is_some();
            let plan = Arc::new(LogoutSettlement {
                cleanup: owned, system, ship, token: transition.entries[0].0,
                snapshot: transition.entries[0].1.clone(), facts: transition.facts.clone(), position: ball.pos, before: Mutex::new(None),
                logoff_possible,
                skipped_logoff: std::sync::atomic::AtomicBool::new(false),
                before_logoff: Mutex::new(None),
            });
            if s.defer_condition_transition(transition, move |_| async move {
                let Some(world) = plan.cleanup.world.upgrade() else {
                    plan.cleanup.finish(Err("world stopped during logout".into())).await;
                    return;
                };
                let baseline = plan.cleanup.db.transaction(|tx, _: &mut ()| {
                    let plan = plan.clone();
                    Box::pin(async move {
                        characters::selection::lock_character(tx, char_id).await?;
                        items::ship::lock_reload_ship(tx, ship).await?;
                        let state = plan.read_checkpoint(tx).await?;
                        let timestamps = if plan.logoff_possible { Some(characters::selection::logout_timestamps(tx, char_id).await?) } else { None };
                        Ok::<_, crate::db::Error>((state, timestamps))
                    })
                }, |value, ()| async move { Ok(value) }).await;
                let ((position, rows), timestamps) = match baseline {
                    Ok(((true, position, rows), timestamps)) => ((position, rows), timestamps),
                    Ok(((false, _, _), _)) => {
                        let _ = plan.retire(&world).await;
                        drop(gate);
                        drop(market);
                        plan.cleanup.finish(Err("ship moved before logout checkpoint".into())).await;
                        return;
                    }
                    Err(_) => {
                        let (token, generation) = (plan.token, plan.snapshot.generation);
                        world.with(system, move |s| s.cancel_combat_transition(ship, token, generation)).await;
                        drop(gate);
                        drop(market);
                        plan.cleanup.finish(Err("ship checkpoint unavailable before logout".into())).await;
                        return;
                    }
                };
                *plan.before.lock().unwrap() = Some((position, rows));
                *plan.before_logoff.lock().unwrap() = timestamps;
                let mut gate = Some(gate);
                let mut market = Some(market);
                let result: Result<bool, CallError> = plan.cleanup.db.transaction(
                    |tx, outbox: &mut crate::db::outbox::Outbox| {
                        let snapshot = plan.snapshot.clone();
                        let position = plan.position;
                        let world = world.clone();
                        let facts = plan.facts.clone();
                        plan.skipped_logoff.store(false, std::sync::atomic::Ordering::SeqCst);
                        let decision_plan = plan.clone();
                        Box::pin(async move {
                            characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
                            if !items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?
                                || !crate::dao::combat::victim_current(tx, char_id, ship, system).await.map_err(db_err)? {
                                return Err(CallError::Internal(anyhow::anyhow!("ship is no longer available for logout")));
                            }
                            let written = super::combat::checkpoint_conditions_in(tx, outbox, system, std::slice::from_ref(&snapshot), facts.as_deref()).await?;
                            if snapshot.revision != snapshot.saved_revision && !written.contains(&ship) {
                                return Err(CallError::Internal(anyhow::anyhow!("ship condition was not checkpointed")));
                            }
                            if items::positions::save(tx, ship, &position).await.map_err(db_err)? != 1 {
                                return Err(CallError::Internal(anyhow::anyhow!("ship position was not checkpointed")));
                            }
                            if let Some(user_id) = decision_plan.logoff_user(user_id, &world) {
                                characters::selection::logoff(tx, char_id).await.map_err(db_err)?;
                                outbox.event(crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id });
                            }
                            Ok(written)
                        })
                    }, |written, outbox| {
                        let plan = &plan;
                        let world = &world;
                        let gate = &mut gate;
                        let market = &mut market;
                        async move {
                        if let Some(facts) = plan.facts.clone() {
                            let written = written.into_iter().collect();
                            let _ = world.with(system, move |s| facts.acknowledge(s, &written)).await;
                        }
                        let removed = plan.retire(&world).await == Some(true);
                        if removed && world.online_session(char_id) == plan.cleanup.expected {
                            super::combat::reset_pilot(&world, char_id, Some((system, ship)));
                        }
                        if removed {
                            drop(gate.take());
                            drop(market.take());
                            plan.cleanup.finish(Ok(())).await;
                        }
                        crate::db::outbox::dispatch_world(removed, outbox, world)
                        }
                    },
                ).await;
                if matches!(result, Ok(true)) {
                    return;
                }
                let callback = plan.callback();
                let (token, generation) = (plan.token, plan.snapshot.generation);
                let pending = world.with(system, move |s| s.mark_pending_transition(ship, token, generation, callback)).await.unwrap_or(false);
                drop(gate);
                drop(market);
                if pending { plan.reconcile().await; }
                else { plan.cleanup.finish(Err("system stopped during logout settlement".into())).await; }
            }) { LogoutAdmission::Submitted } else { LogoutAdmission::Unavailable }
        }).await;
        match submitted {
            Some(LogoutAdmission::Submitted) => {}
            Some(LogoutAdmission::NoMembership) => cleanup.finish_without_checkpoint(user_id).await,
            _ => cleanup.finish(Err("ship or system unavailable for logout".into())).await,
        }
    } else {
        if char_id != 0 { super::combat::reset_pilot(world, char_id, space_system.zip(identity.ship_id)); }
        cleanup.finish_without_checkpoint(user_id).await;
    }
    receiver.await.map_err(|_| anyhow::anyhow!("logout worker unavailable"))?.map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests;
