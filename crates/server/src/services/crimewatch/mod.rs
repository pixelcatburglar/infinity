//! Crimewatch: combat timers, criminal and disapproval flags, engagements,
//! safety level, security status, CONCORD and killmails
//! (eve/client/script/ui/services/crimewatchSvc.py, crimewatch.const).
//!
//! Activating a weapon on another pilot's ship is legal against opponents in an active war,
//! criminals or suspects, configured outlaws, pilots you are engaged with, and anywhere outside empire space.
//! Otherwise it is a criminal act: the aggressor loses security status and is flagged criminal; in high-sec
//! CONCORD destroys their ship after a response time that grows as security falls, unless
//! CONCORD responses are disabled. Aggression starts weapons and PvP timers and a mutual
//! engagement regardless; their durations are operator policies.
//! Entosis Link activation also starts the weapons timer without player aggression
//! (CCP Help Center, article 203729281).
//!
//! A lost player ship writes a killmail (game.killmails) naming attackers within the configured
//! window; an illegal kill costs those aggressors more security status. Killing NPCs that carry
//! a bounty slowly raises it. Security adjustments and CONCORD response delays are configurable.
//! CONCORD responses use the crime system's actor timer; character expiry timers
//! follow pilots across systems and stations on Tokio tasks. These timers and flags
//! are in memory; at startup active persisted kill rights restore their suspect flags
//! without extending their deadlines. Other timers and flags are lost on restart.
//! Six character deadlines (weapons, PvP, NPC, criminal, suspect and
//! disapproval) follow pilots across systems and stations on one Tokio expiry
//! worker per live (character, kind); start, restart, shorten and clear switch
//! its generation so a late worker can never publish the old state, while a
//! natural refresh keeps the running worker and its later deadline. Criminal
//! and suspect share the client's fourth timer slot with criminal winning;
//! disapproval has its own timer plus a system flag roster that `GetClientStates`
//! also catches up because the client's session change forgets only criminal
//! flags. Accepted damage involving an explicit NPC on either side refreshes
//! the NPC timer; forced losses and refused or zero damage never do. The
//! safety level is persisted per character in game.characters and written
//! only through the shared setter (the bound `SetSafetyLevel` call and
//! `/cwatch safety`); there is no push, enforcement or safety-related session
//! change. CONCORD responses are generation tickets in `State.concord`:
//! clearing a criminal flag cancels the queued response, a later crime
//! reserves a fresh generation, and both the timer admission and the final
//! damage guard recheck the ticket plus the live ship, membership and ignore
//! state.
//! A pilot's persistent GM NPC-ignore flag only fences the CONCORD admission:
//! a response scheduled before the flag is set arrives as a no-op while the pilot
//! stays ignored; clearing it does not request CONCORD for crimes committed while
//! it was set, and ordinary crime policy is unchanged.
//! Killmail records, kill-report notifications and security penalties commit together;
//! damage contributions remain in memory until that transaction commits.
//! Player ship and capsule map losses commit in the same transaction as the killmail.
//! System security comes from the startup SDE map; character security is persisted in game data.
pub(crate) mod control;
mod corp_registry;
mod crimewatch;
pub(crate) mod killmails;
pub(crate) mod timers;
mod war_statistic_mgr;

use super::{db_err, keyval, Call, CallError, CallResult};
use killmails::killmails;
use crate::client::{filetime_now, Client};
use infinity_core::consts::FILETIME_SECOND;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use infinity_core::messages;
use crate::space::{seconds_to_ms, World};
use marshal::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use tracing::{info, warn};

// crimewatch.const
const WEAPONS_IDLE: i64 = 100;
const WEAPONS_TIMER: i64 = 102;
const PVP_IDLE: i64 = 200;
const PVP_TIMER: i64 = 202;
const CRIMINAL_IDLE: i64 = 300;
const CRIMINAL_TIMER: i64 = 303;
const SUSPECT_TIMER: i64 = 304;
const NPC_IDLE: i64 = 400;
const NPC_TIMER: i64 = 402;
const DISAPPROVAL_IDLE: i64 = 500;
const DISAPPROVAL_TIMER: i64 = 502;

/// Existing CONCORD response formula's minimum security input, distinct from the high-sec threshold.
const CONCORD_MIN_SECURITY: f64 = 0.5;
// notificationconst
const NOTIFICATION_KILL_REPORT_VICTIM: i32 = 140;
const NOTIFICATION_KILL_REPORT_FINAL_BLOW: i32 = 141;
const NOTIFICATION_GROUP_WAR: i32 = 8;

#[derive(Default)]
struct Pilot {
    weapons_until: i64,
    pvp_until: i64,
    criminal_until: i64,
    suspect_until: i64,
    npc_until: i64,
    disapproval_until: i64,
    /// Whether the character ever held an active disapproval flag this
    /// process; drives the `GetClientStates` idle catch-up.
    had_disapproval: bool,
    /// Current expiry-worker generation per `control::TimerKind` slot.
    timer_generations: [u64; 6],
}

/// An attacker's share of the damage to a ship.
#[derive(Clone, Default, PartialEq)]
struct Contribution {
    damage: f64,
    ship_type: i32,
    weapon_type: i32,
    illegal: bool,
}

#[derive(Default)]
struct State {
    pilots: HashMap<i64, Pilot>,
    /// (a, b) -> until when a may shoot b without consequence.
    engagements: HashMap<(i64, i64), i64>,
    /// Criminals CONCORD is on its way to, by response generation.
    concord: HashMap<i64, u64>,
    /// Next response generation; monotonic so a cancelled ticket can never
    /// collide with a later reservation.
    concord_next: u64,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    crate::poison::lock_or_exit(STATE.get_or_init(Default::default).lock())
}

/// One reserved CONCORD response slot; the generation fences every later use.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ConcordTicket {
    character: i64,
    generation: u64,
}

/// Owns a pending response until its timer runs or is discarded; the Drop
/// releases the slot only while this exact ticket is still current.
struct PendingConcord(ConcordTicket);

impl Drop for PendingConcord {
    fn drop(&mut self) {
        let mut st = state();
        if st.concord.get(&self.0.character) == Some(&self.0.generation) {
            st.concord.remove(&self.0.character);
        }
    }
}

/// When the pilot's weapons timer runs out, if it is running.
pub fn weapons_timer(char_id: i64) -> Option<i64> {
    let until = state().pilots.get(&char_id).map(|p| p.weapons_until)?;
    (until > filetime_now()).then_some(until)
}

fn is_criminal(char_id: i64, now: i64) -> bool {
    state().pilots.get(&char_id).is_some_and(|p| p.criminal_until > now || p.suspect_until > now)
}
/// Whether existing engagement or flags permit retaliation against a player.
pub(crate) fn retaliation_allowed(owner: i64, aggressor: i64, now: i64) -> bool {
    if owner == aggressor { return false; }
    let st = state();
    st.engagements.get(&(owner, aggressor)).is_some_and(|until| *until > now)
        || st.pilots.get(&aggressor).is_some_and(|p| p.criminal_until > now || p.suspect_until > now)
}


pub async fn security_status(db: &(impl Executor + ?Sized), char_id: i64) -> Result<f64, CallError> {
    Ok(crate::dao::characters::crimewatch::security(db, char_id).await.map_err(db_err)?.unwrap_or(0.0))
}

fn system_security(world: &World, system: i64) -> f64 {
    world.sde().map.system(system).and_then(|s| s.security).unwrap_or(0.0)
}

/// Whether `system` is high-security space under `config.crimewatch.highsec_threshold`;
/// unknown systems and missing security rows are not high-sec.
pub(crate) fn highsec_system(sde: &crate::sde::Sde, system: i64) -> bool {
    sde.map.system(system).and_then(|s| s.security)
        .is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold)
}

/// Whether `system` is empire space on the client's security-class boundary:
/// any positive security. Unknown systems and missing security rows are not
/// empire space; the high-sec policy threshold does not apply here.
pub(crate) fn empire_system(sde: &crate::sde::Sde, system: i64) -> bool {
    sde.map.system(system).and_then(|s| s.security).is_some_and(|security| security > 0.0)
}

/// Move a pilot's security status `fraction` of the way to `toward` (+10 or -10).
async fn shift_security<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, fraction: f64, toward: f64) -> Result<f64, CallError> {
    let new = crate::dao::characters::crimewatch::shift_security(db, char_id, fraction, toward).await.map_err(db_err)?;
    outbox.notify_pilot(char_id, "OnSecurityStatusUpdate", vec![Value::Float(new)]);
    Ok(new)
}

/// Set a pilot's security status and queue the normal client notification.
pub(crate) async fn set_security<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, value: f64) -> Result<f64, CallError> {
    let new = crate::dao::characters::crimewatch::set_security(db, char_id, value).await.map_err(db_err)?;
    outbox.notify_pilot(char_id, "OnSecurityStatusUpdate", vec![Value::Float(new)]);
    Ok(new)
}

/// Pilots in space in a system.
async fn pilots_in(world: &World, system: i64) -> Vec<i64> {
    world.with(system, move |s| s.pilots().filter_map(|b| b.char_id).collect()).await.unwrap_or_default()
}

fn timer(state: i64, until: Option<i64>) -> Value {
    Value::tuple([Value::Int(state), until.map(Value::Int).unwrap_or(Value::None)])
}

/// Start or refresh only the weapons timer after an Entosis Link activation.
pub(crate) fn weapons_activate(world: &Arc<World>, character: i64) {
    timers::activate(world, character, control::TimerKind::Weapons,
        timers::natural_deadline(control::TimerKind::Weapons, filetime_now()), true);
}

/// `attacker` activated a weapon on `victim`'s ship in `system`.
/// Returns whether this aggression was illegal before engagement state changed.
pub async fn aggress(db: &Db, world: &Arc<World>, system: i64, attacker: i64, victim: i64) -> Result<bool, CallError> {
    let illegal = classify(db, world, system, attacker, victim).await?;
    apply(db, world, system, attacker, victim, illegal).await?;
    Ok(illegal)
}

/// Classify an admission without changing timers or engagements.
pub(crate) async fn classify(db: &Db, world: &Arc<World>, system: i64, attacker: i64, victim: i64) -> Result<bool, CallError> {
    if attacker == victim { return Ok(false); }
    let now = filetime_now();
    let policy = &crate::config::game().crimewatch;
    let system_sec = system_security(world, system);
    let victim_sec = security_status(db, victim).await?;
    let engaged = state().engagements.get(&(attacker, victim)).is_some_and(|until| *until > now);
    Ok(!(system_sec <= 0.0
        || is_criminal(victim, now)
        || victim_sec <= policy.outlaw_threshold
        || engaged
        || super::wars::classification::legal(db, attacker, victim, now).await?))
}

/// Apply an already classified admission without reclassifying it.
pub(crate) async fn apply(db: &Db, world: &Arc<World>, system: i64, attacker: i64, victim: i64, illegal: bool) -> Result<(), CallError> {
    if attacker == victim { return Ok(()); }
    let now = filetime_now();
    let policy = &crate::config::game().crimewatch;
    let system_sec = system_security(world, system);
    let (weapons, pvp, until) = (
        timers::natural_deadline(control::TimerKind::Weapons, now),
        timers::natural_deadline(control::TimerKind::Pvp, now),
        timers::natural_deadline(control::TimerKind::Criminal, now),
    );
    let engagement = crate::client::filetime_deadline_from_seconds_saturating(now, policy.engagement_secs);
    let mut spawns = [None; 4];
    let mut spawn_count = 0usize;
    let (became_criminal, ticket) = {
        let mut st = state();
        let became = illegal && st.pilots.get(&attacker).is_none_or(|p| p.criminal_until <= now);
        // Immediate pilot notices publish inside the same critical section as
        // the deadline write, so a racing clear or off cannot be overwritten
        // by a stale running notice.
        let mut write = |st: &mut State, char_id: i64, kind: control::TimerKind, deadline: i64| {
            if let Some(generation) = timers::write_deadline(st, char_id, kind, deadline, true, now) {
                spawns[spawn_count] = Some((char_id, kind, generation));
                spawn_count += 1;
            }
            let (event, state_code, until) = timers::notice(st, char_id, kind, now);
            world.notify_pilot(char_id, event, vec![Value::Int(state_code), until.map(Value::Int).unwrap_or(Value::None)]);
        };
        write(&mut st, attacker, control::TimerKind::Weapons, weapons);
        write(&mut st, attacker, control::TimerKind::Pvp, pvp);
        if illegal { write(&mut st, attacker, control::TimerKind::Criminal, until); }
        write(&mut st, victim, control::TimerKind::Pvp, pvp);
        st.engagements.insert((victim, attacker), engagement);
        st.engagements.insert((attacker, victim), engagement);
        world.notify_pilot(attacker, "OnCrimewatchEngagementCreated", vec![Value::Int(victim), Value::Int(engagement)]);
        world.notify_pilot(victim, "OnCrimewatchEngagementCreated", vec![Value::Int(attacker), Value::Int(engagement)]);
        // Reserve and own the ticket inside the same critical section as the
        // flag write: an off or clear removes flag and ticket together, the
        // owner cleans up itself on any abandoned path, and a stale dispatch
        // of a since-cancelled ticket no-ops at admission.
        let ticket = if illegal {
            reserve_concord(&mut st, policy.concord_enabled, system_sec, policy.highsec_threshold, attacker).map(PendingConcord)
        } else {
            None
        };
        (became, ticket)
    };
    for (char_id, kind, generation) in spawns.into_iter().flatten() {
        timers::spawn_expiry(world, char_id, kind, generation);
    }
    if !illegal {
        return Ok(());
    }
    if let Err(error) = super::bounty::kill_rights::earn(db, world, victim, attacker).await {
        match error {
            CallError::Internal(error) => warn!("crimewatch: earning kill right failed: {error:#}"),
            CallError::User { msg, .. } => warn!("crimewatch: earning kill right refused: {msg}"),
        }
    }
    let mut outbox = Outbox::default();
    shift_security(db, &mut outbox, attacker, policy.security_loss_aggression, -10.0).await?;
    dispatch_world((), outbox, world)?;
    info!("crimewatch: {attacker} committed a criminal act against {victim} in {system} (security {system_sec:.1})");
    if became_criminal {
        timers::announce_flag(db, world, attacker, None).await;
    }
    if let Some(pending) = ticket {
        dispatch_concord(world.clone(), system, pending, system_sec);
    }
    Ok(())
}

/// Reserve a CONCORD response for `character` under the already-held state
/// guard when the system qualifies and none is pending; a cancelled or
/// consumed slot can be reserved again under a fresh generation, and `None`
/// also means the generation counter is exhausted (the admission is refused
/// rather than reusing an old generation).
fn reserve_concord(st: &mut State, enabled: bool, security: f64, highsec_threshold: f64, character: i64) -> Option<ConcordTicket> {
    if !(enabled && security >= highsec_threshold) || st.concord.contains_key(&character) { return None; }
    let generation = st.concord_next;
    st.concord_next = st.concord_next.checked_add(1)?;
    st.concord.insert(character, generation);
    Some(ConcordTicket { character, generation })
}

/// Cancel the character's queued response; a new crime reserves afresh.
fn cancel_concord(character: i64) {
    state().concord.remove(&character);
}

/// Whether `ticket` is still the live reservation for its character.
fn concord_ticket_current(ticket: ConcordTicket) -> bool {
    state().concord.get(&ticket.character) == Some(&ticket.generation)
}

/// Whether an outstanding CONCORD response ticket is bound for `character`.
/// The reserved ticket is the only pursuit authority: suspect, criminal or
/// security status alone say nothing about an inbound response, and no
/// faction-police producer exists to consult.
pub(crate) fn concord_pursuit(character: i64) -> bool {
    state().concord.contains_key(&character)
}

/// Resolve the effective flag without hiding a still-active criminal or suspect.
fn effective_flag(pilot: &Pilot, now: i64) -> (usize, i64, Option<i64>) {
    if pilot.criminal_until > now {
        (2, CRIMINAL_TIMER, Some(pilot.criminal_until))
    } else if pilot.suspect_until > now {
        (1, SUSPECT_TIMER, Some(pilot.suspect_until))
    } else {
        (0, CRIMINAL_IDLE, None)
    }
}

/// Apply a suspect flag for a paid or owner-activated kill right.
pub async fn flag_suspect(db: &Db, world: &Arc<World>, char_id: i64, until: i64) {
    timers::set(world, db, char_id, control::TimerKind::Suspect, until, true).await;
}

/// CONCORD destroys the criminal's ship after the configured security-dependent delay.
fn dispatch_concord(world: Arc<World>, system: i64, pending: PendingConcord, security: f64) {
    let policy = &crate::config::game().crimewatch;
    let delay_seconds = concord_delay_seconds(security, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs);
    let weak_world = Arc::downgrade(&world);
    world.schedule(system, seconds_to_ms(delay_seconds as f64), concord_response(pending, system, weak_world));
}

fn concord_delay_seconds(security: f64, base: i64, slope: i64) -> u64 {
    (base as f64 + (1.0 - security.clamp(CONCORD_MIN_SECURITY, 1.0)) * slope as f64) as u64
}

/// Whether the criminal's original ship is still piloted by them, in the same
/// combat membership generation, and not NPC-ignored; fences both the timer's
/// early admission and the final guarded application of a scheduled response.
fn concord_target_current(s: &crate::space::System, character: i64, ship: i64, generation: u64) -> bool {
    s.pilot_ball(character).is_some_and(|ball| ball.id == ship)
        && s.combat_membership_generation(ship) == Some(generation)
        && !s.npc_target_ignored(ship)
}

fn concord_response(pending: PendingConcord, system: i64, weak_world: Weak<World>) -> impl FnOnce(&mut crate::space::System) + Send + 'static {
    move |s| {
        let ticket = pending.0;
        let criminal = ticket.character;
        if !concord_ticket_current(ticket) { return; }
        let current = s.pilot_ball(criminal).and_then(|b|
            s.combat_membership_generation(b.id).map(|generation| (b.id, generation)))
            .filter(|&(ship, generation)| concord_target_current(s, criminal, ship, generation));
        if let Some((ship, generation)) = current {
            s.defer(async move {
                // The reservation stays held until the deferred apply resolves;
                // a cancelled ticket can never reach the guarded loss below.
                let _pending = pending;
                if let Some(world) = weak_world.upgrade() {
                    match super::combat::damage::apply_damage_guarded(world.db(), &world, system, ship, [0.0; 4], super::combat::damage::DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Natural), ..Default::default() }, true,
                        move |s| concord_ticket_current(ticket) && concord_target_current(s, criminal, ship, generation)).await {
                        Ok(Some(outcome)) if outcome.lethal => world.notify_pilot(criminal, "OnRemoteMessage", vec![Value::str(messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(messages::CONCORD_DESTROYED_SHIP_FOR_CRIME))])]),
                        Ok(_) => {},
                        Err(error) => warn!("crimewatch: CONCORD ship loss failed: {error:?}"),
                    }
                }
            });
        }
    }
}

/// What was on a lost ship, for its killmail: (typeID, flagID, singleton, quantity).
pub type Lost = Vec<(i32, i32, bool, i64)>;

/// Write the killmail and penalties in the same transaction as the loss.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ship_lost_in<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, prices: &crate::sde::prices::Prices,
    system: i64, victim: i64, victim_corp: i64, ship: i64, ship_type: i32, items: &Lost,
    source: &super::combat::damage::DamageSource, now: i64,
    contributions: &[crate::space::combat::ContributionEntry],
) -> Result<(i64, f64, HashMap<i32, f64>, i64), CallError> {
    let window = crate::config::game().crimewatch.killmail_attacker_window_secs.saturating_mul(FILETIME_SECOND);
    let cutoff = now.saturating_sub(window);
    let mut attackers: Vec<_> = contributions.iter().filter(|r| r.last_damage_at >= cutoff).map(|r| (r.attacker_id, Contribution {
        damage: r.damage, ship_type: r.source_ship_type.unwrap_or(0),
        weapon_type: r.weapon_type_id.unwrap_or(0), illegal: r.illegal,
    })).collect();
    write_killmail(ex, outbox, prices, system, victim, victim_corp, ship_type, items, &mut attackers, now, Some(ship), source).await
}

#[allow(clippy::too_many_arguments)]
async fn write_killmail<X: Send>(
    db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, prices: &crate::sde::prices::Prices,
    system: i64, victim: i64, victim_corp: i64, ship_type: i32, items: &Lost,
    attackers: &mut [(i64, Contribution)], now: i64, victim_item_id: Option<i64>,
    source: &super::combat::damage::DamageSource,
) -> Result<(i64, f64, HashMap<i32, f64>, i64), CallError> {
    let final_blow = source.attacker_id.and_then(|id| attackers.iter().find(|(ch, _)| *ch == id).cloned());
    let mut blob = String::from("<doc><attackers>");
    let mut affiliations = Vec::with_capacity(attackers.len());
    for (char_id, c) in attackers.iter() {
        let info = crate::dao::corporations::roles::info(db, *char_id).await.map_err(db_err)?;
        let corp = info.as_ref().map(|info| info.corporation_id).unwrap_or(0);
        if let Some(info) = info {
            affiliations.push(super::wars::classification::Affiliation {
                character: *char_id, corporation: info.corporation_id, alliance: info.alliance_id,
            });
        }
        let sec = security_status(db, *char_id).await?;
        blob += &format!("<a c={char_id} r={corp} s={} w={} d={} t={sec:.1} />", c.ship_type, c.weapon_type, c.damage.round() as i64);
    }
    blob += "</attackers><items>";
    for (type_id, flag, singleton, quantity) in items {
        blob += &format!("<i t={type_id} f={flag} s={} d=0 x={quantity} />", *singleton as i32);
    }
    blob += "</items></doc>";
    let mut price: HashMap<i32, f64> = HashMap::with_capacity(items.len() + 1);
    for type_id in items.iter().map(|item| item.0).chain(std::iter::once(ship_type)) {
        if let Some(row) = prices.get(type_id) { price.insert(type_id, row.average); }
    }
    let observed_at = crate::client::filetime_now();
    let isk_lost = price.get(&ship_type).copied().unwrap_or(0.0) + items.iter().map(|(t, _, _, q)| price.get(t).copied().unwrap_or(0.0) * *q as f64).sum::<f64>();
    let damage_taken: f64 = attackers.iter().map(|(_, c)| c.damage).sum();
    let final_char = source.attacker_id;
    let final_damage = final_blow.as_ref().map_or(0.0, |(_, c)| c.damage);
    let final_corp = affiliations.iter().find(|a| Some(a.character) == final_char).map(|a| a.corporation);
    let final_alliance = affiliations.iter().find(|a| Some(a.character) == final_char).and_then(|a| a.alliance);
    let victim_alliance = crate::dao::alliances::corporation_membership(db, victim_corp).await.map_err(db_err)?.map(|membership| membership.alliance);
    let final_sec = match final_char {
        Some(ch) => security_status(db, ch).await?,
        None => 0.0,
    };
    let involved: Vec<i64> = attackers.iter().map(|(ch, _)| *ch).collect();
    let kill_id = crate::dao::killmails::insert(db, &crate::dao::killmails::NewKillmail {
        now, system, victim, victim_item_id, victim_corp, victim_alliance, ship_type, damage_taken, final_char, final_corp, final_alliance,
        final_ship_type: source.source_ship_type.unwrap_or(0), final_weapon_type: source.weapon_type_id.unwrap_or(0), final_damage,
        final_sec, isk_lost, involved: &involved, blob: &blob,
    }).await.map_err(db_err)?;
    super::map::events::player_lost(db, kill_id, system, ship_type == infinity_core::consts::TYPE_CAPSULE, now).await?;
    super::wars::classification::index(db, kill_id, victim_corp, victim_alliance, &affiliations, now).await?;
    super::bounty::kill_rights::consume(db, outbox, victim, now).await?;
    if let Some(final_char) = final_char {
        super::bounty::payout::schedule(db, super::bounty::payout::Payload {
            killmail_id: kill_id, victim, victim_corp, ship_type, final_char, isk_lost,
        }, now).await?;
    }

    // Kill reports (notificationconst groupWar): the victim's, and the final blow's.
    super::mail::notify(db, outbox, victim, NOTIFICATION_KILL_REPORT_VICTIM, NOTIFICATION_GROUP_WAR, victim, &format!("{{killMailHash: '', killMailID: {kill_id}}}"))
        .await?;
    if let Some(killer) = final_char {
        let data = format!("{{killMailHash: '', killMailID: {kill_id}, victimID: {victim}, victimShipTypeID: {ship_type}}}");
        super::mail::notify(db, outbox, killer, NOTIFICATION_KILL_REPORT_FINAL_BLOW, NOTIFICATION_GROUP_WAR, victim, &data).await?;
    }
    attackers.sort_unstable_by_key(|(char_id, _)| *char_id);
    for (char_id, c) in attackers {
        if c.illegal {
            shift_security(db, outbox, *char_id, crate::config::game().crimewatch.security_loss_kill, -10.0).await?;
        }
    }
    Ok((kill_id, isk_lost, price, observed_at))
}

/// Settle one character's mixed NPC security gain in the caller's transaction:
/// legacy kills use this process's validated rate, already accepted new gain
/// stays frozen, and the result is one security UPDATE and notice.
pub(crate) async fn settle_npc_gain<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, character: i64,
    total_count: u64, legacy_count: u64, new_gain: crate::npc_security::SecurityGain,
) -> Result<(), CallError> {
    if total_count == 0 { return Ok(()); }
    if legacy_count > total_count {
        return Err(CallError::Internal(anyhow::anyhow!("npc bounty legacy count exceeds its total")));
    }
    let legacy = crate::npc_security::SecurityGain::for_legacy(
        crate::config::game().crimewatch.security_gain_npc_kill, legacy_count);
    shift_security(ex, outbox, character, legacy.merge(new_gain).fraction(), 10.0).await?;
    Ok(())
}




pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "crimewatch" => crimewatch::dispatch(c, call).await,
        "corpRegistry" => corp_registry::dispatch(c, call).await,
        "warStatisticMgr" => war_statistic_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;
