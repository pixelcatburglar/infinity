//! Bounded private visual-effect previews for the `/effect` GM command,
//! delivered as OnSpecialFX destiny events consumed by the client's
//! fxSequencer. The catalogue mirrors the client's thirty-two effect names:
//! twenty-four are previewable and eight are refused by class because their
//! triggers share real client state (cloak overlays, the siege-mode model
//! flag, abyssal travel transitions and an empty trace-gate effect).
//! Effect identities come from this module's compiled table; the SDE is
//! consulted only for the source ship's category, the target ball's model
//! class and the static types a weapon pulse may carry.
//!
//! Each preview goes out only over the issuing GM's existing destiny
//! observer channel through the shared OnSpecialFX encoder, with the
//! classification default duration and repeat zero; the server keeps no
//! timer. Fourteen supported effects keep a sent handle, at most one per
//! effect per real source ship, so a later stop removes exactly the
//! trigger the command added. Microjump and the nine weapon effects are
//! one-shot pulses: each command sends one start, nothing is recorded and
//! stop cannot cancel them. The records say what was sent, never that the
//! client still renders it, and are dropped with the ball, the observer or
//! the actor; a sent start guarantees delivery to the observer, not
//! visible rendering.
//! Preview events share the actor's ordered pending delivery.
//!
//! Weapon pulses carry a real static type resolved from the loaded data:
//! candidates are every loaded type, of any category, whose dogma effect
//! list carries the catalogue GUID as one exact comma-separated token. An
//! omitted `moduleType=` accepts a sole candidate, refuses when none is
//! linked and lists every candidate id when several are; an explicit value
//! must itself be a candidate unless the data links none at all, in which
//! case a known module type with at least one own attribute row stands in
//! and the reply is flagged unchecked. An own `damageDelayDuration` row
//! decides only whether the client renders a beam without a target impact;
//! it is never an admission gate. A targeted preview without an explicit
//! ball falls back to the caller's lock set only when it holds exactly one
//! completed lock. Accepted client-side behaviours (link icons on the
//! beam previews, the microjump camera move, whiteout and firing-locator
//! limitations) are operator limitations, not refusals.
//!
//! Not supported: duration, repeat or graphicInfo arguments, looping or
//! repeated triggers, previews broadcast to other observers or to the
//! target's own client, implicit nearest-station targeting, and every
//! refused name.

use super::*;
use crate::space::system::observers::{BallToken, DestinyAudience, ObserverToken};
use crate::services::combat::targeting_charges;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP};

/// The twenty-four previewable effects in catalogue order. The first
/// fourteen hold sent-handle slots, so `sent_slot` is just the declaration
/// order; the last ten are one-shot pulses and never occupy a record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VisualEffect {
    JumpIn,
    JumpDriveIn,
    AbyssalReentrySplash,
    JumpDriveInBO,
    JumpPortal,
    JumpPortalBO,
    CapsuleFlare,
    Whiteout,
    RemoteArmourRepair,
    ShieldTransfer,
    DataLink,
    MainBankHackingBeam,
    ReserveBankHackingBeam,
    CelestialBeam,
    MicroJump,
    TurboLaser,
    LanceAmarr,
    LanceCaldari,
    LanceGallente,
    LanceMinmatar,
    SuperWeaponAmarr,
    SuperWeaponCaldari,
    SuperWeaponGallente,
    SuperWeaponMinmatar,
}
impl VisualEffect {
    /// Handle-bearing effects come first; `CelestialBeam` is the last one.
    const SLOT_COUNT: usize = Self::CelestialBeam as usize + 1;
    /// The fixed catalogue row describing this effect.
    pub(crate) fn entry(self) -> &'static Entry {
        CATALOGUE.iter().find(|entry| entry.disposition == Ok(self)).expect("catalogue covers every previewable effect")
    }
    /// Whether a preview requires a real same-system target ball. Every
    /// effect outside the nine self effects targets another ball.
    pub(crate) fn targeted(self) -> bool {
        !matches!(self, Self::JumpIn | Self::JumpDriveIn | Self::AbyssalReentrySplash
            | Self::JumpDriveInBO | Self::JumpPortal | Self::JumpPortalBO
            | Self::CapsuleFlare | Self::Whiteout | Self::MicroJump)
    }
    /// Whether a send is a one-shot pulse that is never recorded and can
    /// never be stopped once delivered to the observer.
    pub(crate) fn one_shot(self) -> bool { self.sent_slot().is_none() }
    /// Whether the client firing effect reads a firing static type id off
    /// the wire. The nine weapon pulses do; microjump does not.
    pub(crate) fn needs_module_type(self) -> bool { self.one_shot() && self != Self::MicroJump }
    /// The fixed sent-handle slot of a handle effect; pulses have none.
    fn sent_slot(self) -> Option<usize> {
        if (self as usize) < Self::SLOT_COUNT { Some(self as usize) } else { None }
    }
}

/// Refusal classes for the eight non-previewable names. Each class is a
/// shared client-state hazard the slash layer explains to the operator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Refusal {
    /// Cloak triggers reuse the real cloak presentation and a preview stop
    /// cannot safely restore it.
    Cloak,
    /// TriageMode drives the ship model's shared siege-mode presentation.
    Triage,
    /// Abyssal departures hide models and run travel/tunnel presentation
    /// rather than an isolated preview.
    Travel,
    /// LinkedToTraceGate has no playable visual effect.
    Empty,
}

/// One immutable catalogue row: canonical name, the client effect GUID sent
/// on the wire, every recognized alias and the row's disposition.
pub(crate) struct Entry {
    pub name: &'static str,
    pub guid: &'static str,
    pub aliases: &'static [&'static str],
    pub disposition: Result<VisualEffect, Refusal>,
}

/// The full thirty-two-name reference catalogue in registry order. The GUID
/// and every alias of a row share its disposition; no other GUID is admitted.
pub(crate) const CATALOGUE: &[Entry] = &[
    Entry { name: "cloak", guid: "effects.Cloak", aliases: &["cloakfx", "cloak_effect", "effects.cloak"], disposition: Err(Refusal::Cloak) },
    Entry { name: "cloak_no_anim", guid: "effects.CloakNoAmim", aliases: &["cloaknoanim", "cloak_noamim", "cloaknoamim", "effects.cloaknoamim"], disposition: Err(Refusal::Cloak) },
    Entry { name: "cloak_regardless", guid: "effects.CloakRegardless", aliases: &["cloakregardless", "force_cloak", "effects.cloakregardless"], disposition: Err(Refusal::Cloak) },
    Entry { name: "uncloak", guid: "effects.Uncloak", aliases: &["decloak", "effects.uncloak"], disposition: Err(Refusal::Cloak) },
    Entry { name: "triage", guid: "effects.TriageMode", aliases: &["triagemode", "effects.triagemode"], disposition: Err(Refusal::Triage) },
    Entry { name: "microjump", guid: "effects.MicroJumpDriveEngage", aliases: &["mjd", "micro_jump", "microjumpdriveengage", "effects.microjumpdriveengage"], disposition: Ok(VisualEffect::MicroJump) },
    Entry { name: "jump_in", guid: "effects.JumpIn", aliases: &["jumpin", "effects.jumpin"], disposition: Ok(VisualEffect::JumpIn) },
    Entry { name: "jump_drive_in", guid: "effects.JumpDriveIn", aliases: &["jumpdrivein", "effects.jumpdrivein"], disposition: Ok(VisualEffect::JumpDriveIn) },
    Entry { name: "jump_out_abyssal", guid: "effects.JumpOutAbyssal", aliases: &["abyssal_jump_out", "jumpoutabyssal", "effects.jumpoutabyssal"], disposition: Err(Refusal::Travel) },
    Entry { name: "jump_out_abyssal_between", guid: "effects.JumpOutAbyssalBetween", aliases: &["abyssal_jump_between", "jumpoutabyssalbetween", "effects.jumpoutabyssalbetween"], disposition: Err(Refusal::Travel) },
    Entry { name: "abyssal_reentry_splash", guid: "effects.AbyssalReentrySplash", aliases: &["reentry_splash", "abyssalreentrysplash", "effects.abyssalreentrysplash"], disposition: Ok(VisualEffect::AbyssalReentrySplash) },
    Entry { name: "jump_drive_in_bo", guid: "effects.JumpDriveInBO", aliases: &["jumpdriveinbo", "black_ops_jump_in", "effects.jumpdriveinbo"], disposition: Ok(VisualEffect::JumpDriveInBO) },
    Entry { name: "jump_portal", guid: "effects.JumpPortal", aliases: &["jumpportal", "effects.jumpportal"], disposition: Ok(VisualEffect::JumpPortal) },
    Entry { name: "jump_portal_bo", guid: "effects.JumpPortalBO", aliases: &["jumpportalbo", "black_ops_portal", "effects.jumpportalbo"], disposition: Ok(VisualEffect::JumpPortalBO) },
    Entry { name: "capsule_flare", guid: "effects.CapsuleFlare", aliases: &["capsuleflare", "effects.capsuleflare"], disposition: Ok(VisualEffect::CapsuleFlare) },
    Entry { name: "whiteout", guid: "effects.WhiteOut", aliases: &["white_out", "effects.whiteout"], disposition: Ok(VisualEffect::Whiteout) },
    Entry { name: "celestial_beam", guid: "effects.CelestialBeam", aliases: &["beam", "celestialbeam", "effects.celestialbeam"], disposition: Ok(VisualEffect::CelestialBeam) },
    Entry { name: "remote_armour_repair", guid: "effects.RemoteArmourRepair", aliases: &["remote_armor_repair", "remotearmourrepair", "remotearmorrepair", "effects.remotearmourrepair"], disposition: Ok(VisualEffect::RemoteArmourRepair) },
    Entry { name: "shield_transfer", guid: "effects.ShieldTransfer", aliases: &["shieldtransfer", "effects.shieldtransfer"], disposition: Ok(VisualEffect::ShieldTransfer) },
    Entry { name: "data_link", guid: "effects.DataLink", aliases: &["datalink", "effects.datalink"], disposition: Ok(VisualEffect::DataLink) },
    Entry { name: "linked_to_trace_gate", guid: "effects.LinkedToTraceGate", aliases: &["linkedtotracegate", "trace_gate_link", "effects.linkedtotracegate"], disposition: Err(Refusal::Empty) },
    Entry { name: "main_bank_hacking_beam", guid: "effects.MainBankHackingBeam", aliases: &["mainbankhackingbeam", "main_bank_beam", "effects.mainbankhackingbeam"], disposition: Ok(VisualEffect::MainBankHackingBeam) },
    Entry { name: "reserve_bank_hacking_beam", guid: "effects.ReserveBankHackingBeam", aliases: &["reservebankhackingbeam", "reserve_bank_beam", "effects.reservebankhackingbeam"], disposition: Ok(VisualEffect::ReserveBankHackingBeam) },
    Entry { name: "turbolaser", guid: "effects.TurboLaser", aliases: &["turbo_laser", "effects.turbolaser"], disposition: Ok(VisualEffect::TurboLaser) },
    Entry { name: "lance_amarr", guid: "effects.SuperWeaponLanceAmarr", aliases: &["superweaponlanceamarr", "amarr_lance", "azmaru", "effects.superweaponlanceamarr"], disposition: Ok(VisualEffect::LanceAmarr) },
    Entry { name: "lance_caldari", guid: "effects.SuperWeaponLanceCaldari", aliases: &["superweaponlancecaldari", "caldari_lance", "steel_yari", "effects.superweaponlancecaldari"], disposition: Ok(VisualEffect::LanceCaldari) },
    Entry { name: "lance_gallente", guid: "effects.SuperWeaponLanceGallente", aliases: &["superweaponlancegallente", "gallente_lance", "sarissa", "effects.superweaponlancegallente"], disposition: Ok(VisualEffect::LanceGallente) },
    Entry { name: "lance_minmatar", guid: "effects.SuperWeaponLanceMinmatar", aliases: &["superweaponlanceminmatar", "minmatar_lance", "atgeir", "effects.superweaponlanceminmatar"], disposition: Ok(VisualEffect::LanceMinmatar) },
    Entry { name: "superweapon_amarr", guid: "effects.SuperWeaponAmarr", aliases: &["super_weapon_amarr", "amarr_doomsday", "judgment", "effects.superweaponamarr"], disposition: Ok(VisualEffect::SuperWeaponAmarr) },
    Entry { name: "superweapon_caldari", guid: "effects.SuperWeaponCaldari", aliases: &["super_weapon_caldari", "caldari_doomsday", "oblivion", "effects.superweaponcaldari"], disposition: Ok(VisualEffect::SuperWeaponCaldari) },
    Entry { name: "superweapon_gallente", guid: "effects.SuperWeaponGallente", aliases: &["super_weapon_gallente", "gallente_doomsday", "aurora_ominae", "effects.superweapongallente"], disposition: Ok(VisualEffect::SuperWeaponGallente) },
    Entry { name: "superweapon_minmatar", guid: "effects.SuperWeaponMinmatar", aliases: &["super_weapon_minmatar", "minmatar_doomsday", "gjallarhorn", "effects.superweaponminmatar"], disposition: Ok(VisualEffect::SuperWeaponMinmatar) },
];

/// Reference normalization, produced one byte at a time without allocation:
/// ASCII-lowercase, every run of non-ASCII-alphanumerics becomes one `_`,
/// edge `_` is dropped, and one leading `effects.` prefix is removed first.
fn normalized(raw: &str) -> Normalized<'_> {
    let raw = match raw.get(..8) {
        Some(prefix) if prefix.eq_ignore_ascii_case("effects.") => &raw[8..],
        _ => raw,
    };
    Normalized { chars: raw.chars(), pending: false, emitted: false, deferred: None }
}
struct Normalized<'a> {
    chars: std::str::Chars<'a>,
    /// A separator is owed before the next alphanumeric byte.
    pending: bool,
    /// At least one byte has been emitted, so a separator may appear.
    emitted: bool,
    /// The alphanumeric byte held back while its separator was returned.
    deferred: Option<u8>,
}
impl Iterator for Normalized<'_> {
    type Item = u8;
    fn next(&mut self) -> Option<u8> {
        if let Some(byte) = self.deferred.take() { return Some(byte); }
        for c in self.chars.by_ref() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() {
                if self.pending && self.emitted {
                    self.pending = false;
                    self.deferred = Some(c as u8);
                    return Some(b'_');
                }
                self.emitted = true;
                return Some(c as u8);
            }
            self.pending = true;
        }
        None
    }
}

/// Resolve a typed-in name by canonical name, GUID or recognized alias after
/// reference normalization. There is no fuzzy or substring matching and no
/// arbitrary GUID pass-through; unknown names are not admitted.
pub(crate) fn find(name: &str) -> Option<&'static Entry> {
    let name: Vec<u8> = normalized(name).collect();
    CATALOGUE.iter().find(|entry| normalized(entry.name).eq(name.iter().copied())
        || normalized(entry.guid).eq(name.iter().copied())
        || entry.aliases.iter().any(|alias| normalized(alias).eq(name.iter().copied())))
}

/// One typed `/effect` request; wire values are never chosen by the caller.
/// `target` is the explicit ball id; a targeted play with `None` falls back
/// to the caller's single completed lock. `module_type` is the caller's
/// explicit `moduleType=` and is accepted only by the nine weapon effects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Request {
    Play { effect: VisualEffect, target: Option<i64>, module_type: Option<i32> },
    Stop { effect: VisualEffect, target: Option<i64> },
    StopAll,
}
/// What the actor did for one request. `Sent` means events were queued to
/// the observer's channel; counts are honest stop sends only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Outcome {
    /// `replaced` records that an already-sent handle of the same effect was
    /// stopped in the same batch before the new start. Pulses always report
    /// `replaced: false` because nothing was ever recorded.
    Sent { effect: VisualEffect, target: Option<i64>, module_type: Option<ResolvedModuleType>, replaced: bool },
    Stopped { count: i64 },
}
/// The static type a weapon pulse carries on the wire, plus how it was
/// resolved. `unchecked` marks the zero-link fallback that could not verify
/// the type against the effect; `beam_only` records that the type owns no
/// `damageDelayDuration` row so the client skips the target impact.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ResolvedModuleType {
    pub(crate) type_id: i32,
    pub(crate) unchecked: bool,
    pub(crate) beam_only: bool,
}
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Error {
    /// The session's character no longer pilots `ship`, the observer's ego
    /// disagrees with the requested source, or the recorded handles belong
    /// to a different owning client or character.
    CallerChanged,
    /// No live observer channel exists for this client, or it closed while
    /// the events were being delivered.
    SceneNotReady,
    /// The source ball is gone, cloaked, warping or not an SDE ship; play is
    /// refused while stop may still clear recorded handles.
    SourceUnavailable,
    /// A targeted effect was played without a target ball id and the
    /// caller's lock set holds no completed lock.
    TargetRequired,
    /// The caller's lock set holds more than one completed lock, so no
    /// default target can be chosen.
    TargetAmbiguous,
    /// A target was supplied for a self effect.
    UnexpectedTarget,
    /// The target ball is missing, the source itself, cloaked or a type that
    /// has no ship or station model in this system.
    InvalidTarget,
    /// An explicit stop target disagrees with the recorded one.
    TargetMismatch,
    /// `moduleType=` was omitted and the loaded static data links no type to
    /// the effect GUID; the caller must name a firing static type.
    ModuleTypeRequired,
    /// `moduleType=` was omitted and several static types link the effect
    /// GUID; the complete candidate id list is carried, sorted ascending.
    ModuleTypeAmbiguous(Vec<i32>),
    /// The explicit `moduleType=` is not one of the effect's linked
    /// candidate types.
    ModuleTypeMismatch,
    /// No type links the effect GUID and the explicit `moduleType=` is not
    /// a known module type owning at least one attribute row.
    InvalidModuleType,
    /// `moduleType=` was supplied for an effect that takes none.
    UnexpectedModuleType,
    /// A pulse send is never recorded and cannot be cancelled once sent.
    OneShotCannotStop,
}

/// One recorded OnSpecialFX trigger sent for one handle effect.
#[derive(Clone, Copy)]
struct Sent {
    effect: VisualEffect,
    target: Option<i64>,
}
/// Preview handles recorded for one real source ship: the observing client
/// and pilot character that own them plus the fixed per-effect slots. A
/// handle says what was sent, never that the client still renders it.
pub(super) struct SentPreviews {
    client: i64,
    character: i64,
    slots: [Option<Sent>; VisualEffect::SLOT_COUNT],
}
impl SentPreviews {
    pub(super) fn owned(client: i64, character: i64) -> Self {
        Self { client, character, slots: [None; VisualEffect::SLOT_COUNT] }
    }
}

/// OnSpecialFX start queued for `(ship, module, target, guid)`.
fn play_event(stamp: i64, ship: i64, target: Option<i64>, effect: VisualEffect, module_type: Option<i32>) -> Value {
    special_fx(stamp, ship,
        module_type.map(|type_id| FxModule { item: None, type_id }), target, None, effect.entry().guid,
        false, 1, 1, Value::Float(SPECIAL_FX_UNSPECIFIED_DURATION as f64),
        0, None, 0, graphic_info(effect, true))
}
/// OnSpecialFX stop matching the recorded trigger tuple. Handle effects
/// never carry a module type, so the stop always sends `moduleID` and
/// `moduleTypeID` as `None`.
fn stop_event(stamp: i64, ship: i64, sent: Sent) -> Value {
    special_fx(stamp, ship, None, sent.target, None, sent.effect.entry().guid,
        false, 0, 0, Value::Float(SPECIAL_FX_UNSPECIFIED_DURATION as f64),
        0, None, 0, graphic_info(sent.effect, false))
}
/// Only the celestial beam carries firing state: a Float flag the client
/// reads as `graphicInfo.isFiring`. Every other effect sends `None`.
fn graphic_info(effect: VisualEffect, start: bool) -> Value {
    match effect {
        VisualEffect::CelestialBeam => Value::dict([("isFiring", Value::Float(if start { 1.0 } else { 0.0 }))]),
        _ => Value::None,
    }
}

/// Resolve the static type a weapon pulse carries. Candidates are every
/// loaded type, of any category, whose dogma effects name the catalogue
/// GUID token-for-token. An omitted `moduleType` accepts a sole candidate
/// without allocating, refuses zero and returns the complete sorted
/// candidate id list when several exist. An explicit value is accepted
/// directly when it is itself a candidate; while any other candidate
/// exists it mismatches, and only a total absence of candidates falls back
/// to a known module type owning at least one attribute row.
fn resolve_module_type(sde: &Sde, effect: VisualEffect, requested: Option<i32>) -> Result<Option<ResolvedModuleType>, Error> {
    if !effect.needs_module_type() {
        return if requested.is_some() { Err(Error::UnexpectedModuleType) } else { Ok(None) };
    }
    let guid = effect.entry().guid;
    let linked = |ty: &crate::sde::types::TypeData| has_effect_guid(sde, ty, guid);
    let (ty, unchecked) = match requested {
        Some(id) => match sde.types.get(id) {
            Some(ty) if linked(ty) => (ty, false),
            _ if sde.types.iter().any(|ty| linked(ty)) => return Err(Error::ModuleTypeMismatch),
            // Zero candidates: the link cannot be checked at all, so a
            // known module type owning at least one attribute row stands in.
            Some(ty) if ty.category_id == Some(CATEGORY_MODULE)
                && sde.types.attributes(id).next().is_some() => (ty, true),
            _ => return Err(Error::InvalidModuleType),
        },
        None => {
            let mut candidates = sde.types.iter().filter(|&ty| linked(ty));
            let Some(first) = candidates.next() else { return Err(Error::ModuleTypeRequired) };
            match candidates.next() {
                None => (first, false),
                Some(second) => {
                    let mut ids: Vec<i32> = candidates.map(|ty| ty.type_id).collect();
                    ids.push(first.type_id);
                    ids.push(second.type_id);
                    ids.sort_unstable();
                    return Err(Error::ModuleTypeAmbiguous(ids));
                }
            }
        }
    };
    // The type's own `damageDelayDuration` row decides only whether the
    // client renders a beam without a target impact; it never refuses.
    let beam_only = !sde.dogma.attributes_by_name("damageDelayDuration")
        .any(|attribute| ty.attribute(attribute.attribute_id).is_some());
    Ok(Some(ResolvedModuleType { type_id: ty.type_id, unchecked, beam_only }))
}
/// Whether `ty` links the catalogue GUID: one of its dogma effects carries
/// the exact token inside its comma-separated GUID list. No category,
/// published, delay or fitting condition applies; any type is a candidate.
fn has_effect_guid(sde: &Sde, ty: &crate::sde::types::TypeData, guid: &str) -> bool {
    ty.effects().iter().any(|type_effect| sde.dogma.effect(type_effect.effect_id)
        .and_then(|effect| effect.guid.as_deref())
        .is_some_and(|guids| guids.split(',').map(str::trim).any(|token| token == guid)))
}

impl System {
    /// Run one `/effect` request atomically inside this actor. All checks
    /// against the real membership happen before any event is built; a
    /// refused request changes no record and sends nothing.
    pub(crate) fn gm_effect(&mut self, character: i64, client: i64, ship: i64,
        request: Request, sde: &Sde) -> Result<Outcome, Error> {
        let bound = self.pilot_ball(character)
            .is_some_and(|ball| ball.id == ship && ball.owner_id == character);
        if !bound { return Err(Error::CallerChanged); }
        let Some(ego) = self.observer_ego(client) else { return Err(Error::SceneNotReady) };
        if ego != ship { return Err(Error::CallerChanged) }
        if self.observer_token(client).is_none() { return Err(Error::SceneNotReady) }
        match request {
            Request::Play { effect, target, module_type } => self.gm_effect_play(character, client, ship, effect, target, module_type, sde),
            Request::Stop { effect, target } => self.gm_effect_stop(character, client, ship, effect, target),
            Request::StopAll => self.gm_effect_stop_all(character, client, ship),
        }
    }
    fn gm_effect_play(&mut self, character: i64, client: i64, ship: i64,
        effect: VisualEffect, target: Option<i64>, module_type: Option<i32>, sde: &Sde) -> Result<Outcome, Error> {
        let ball = self.ball(ship).ok_or(Error::SourceUnavailable)?;
        let ship_type = sde.types.get(ball.type_id).and_then(|ty| ty.category_id) == Some(CATEGORY_SHIP);
        if ball.cloak != 0 || ball.mode == MODE_WARP || !ship_type { return Err(Error::SourceUnavailable); }
        let module_type = resolve_module_type(sde, effect, module_type)?;
        let target = match (effect.targeted(), target) {
            (false, Some(_)) => return Err(Error::UnexpectedTarget),
            (false, None) => None,
            (true, supplied) => {
                let id = match supplied {
                    Some(id) => id,
                    None => match targeting_charges::sole_target(character) {
                        Ok(Some(id)) => id,
                        Ok(None) => return Err(Error::TargetRequired),
                        Err(()) => return Err(Error::TargetAmbiguous),
                    },
                };
                Some(self.gm_effect_target(character, ship, id, sde)?)
            }
        };
        // A record owned by another connection or character is stale: stop
        // what it sent on its own observer, then send fresh for the caller.
        if self.gm_previews.get(&ship).is_some_and(|rec| rec.client != client || rec.character != character) {
            self.gm_effect_release(ship);
        }
        let stamp = self.current_time;
        let Some(slot) = effect.sent_slot() else {
            // A pulse send is never recorded; repeating the command is just
            // another explicit send, not a replay of stored state.
            let events = vec![play_event(stamp, ship, target, effect, module_type.map(|m| m.type_id))];
            if !self.observer_token(client).is_some_and(|token| self.emit_destiny(DestinyAudience::Observer(token), events)) {
                self.gm_previews.remove(&ship);
                return Err(Error::SceneNotReady);
            }
            return Ok(Outcome::Sent { effect, target, module_type, replaced: false });
        };
        let old = self.gm_previews.get(&ship).and_then(|rec| rec.slots[slot]);
        let mut events = Vec::with_capacity(2);
        if let Some(old) = old { events.push(stop_event(stamp, ship, old)); }
        events.push(play_event(stamp, ship, target, effect, module_type.map(|m| m.type_id)));
        if !self.observer_token(client).is_some_and(|token| self.emit_destiny(DestinyAudience::Observer(token), events)) {
            self.gm_previews.remove(&ship);
            return Err(Error::SceneNotReady);
        }
        let rec = self.gm_previews.entry(ship).or_insert_with(|| SentPreviews::owned(client, character));
        let replaced = rec.slots[slot].replace(Sent { effect, target }).is_some();
        Ok(Outcome::Sent { effect, target, module_type, replaced })
    }
    /// A targeted preview's destination: a different real ball in this actor,
    /// visible to the caller, whose type the SDE confirms as a ship or which
    /// is a known station: the only models this entry point may name.
    fn gm_effect_target(&self, character: i64, ship: i64, target: i64, sde: &Sde) -> Result<i64, Error> {
        if target <= 0 || target == ship || !self.visible_to(character, target) { return Err(Error::InvalidTarget); }
        let ball = self.ball(target).ok_or(Error::InvalidTarget)?;
        let ship_type = sde.types.get(ball.type_id).and_then(|ty| ty.category_id) == Some(CATEGORY_SHIP);
        if !ship_type && sde.map.station(target).is_none() { return Err(Error::InvalidTarget); }
        Ok(target)
    }
    fn gm_effect_stop(&mut self, character: i64, client: i64, ship: i64,
        effect: VisualEffect, target: Option<i64>) -> Result<Outcome, Error> {
        if effect.one_shot() { return Err(Error::OneShotCannotStop) }
        if target.is_some() && !effect.targeted() { return Err(Error::UnexpectedTarget) }
        let Some(rec) = self.gm_previews.get(&ship) else { return Ok(Outcome::Stopped { count: 0 }) };
        // Only the owner may stop what it sent; a second observer of the
        // same ego cannot cancel the first observer's handles.
        if rec.client != client || rec.character != character { return Err(Error::CallerChanged) }
        let slot = effect.sent_slot().expect("a stopped effect always owns a slot");
        let Some(sent) = rec.slots[slot] else { return Ok(Outcome::Stopped { count: 0 }) };
        if let Some(target) = target {
            if sent.target != Some(target) { return Err(Error::TargetMismatch) }
        }
        let events = vec![stop_event(self.current_time, ship, sent)];
        if !self.observer_token(client).is_some_and(|token| self.emit_destiny(DestinyAudience::Observer(token), events)) {
            self.gm_previews.remove(&ship);
            return Err(Error::SceneNotReady);
        }
        self.gm_effect_slot_clear(ship, slot);
        Ok(Outcome::Stopped { count: 1 })
    }
    fn gm_effect_stop_all(&mut self, character: i64, client: i64, ship: i64) -> Result<Outcome, Error> {
        let Some(rec) = self.gm_previews.get(&ship) else { return Ok(Outcome::Stopped { count: 0 }) };
        if rec.client != client || rec.character != character { return Err(Error::CallerChanged) }
        let stamp = self.current_time;
        let events: Vec<Value> = rec.slots.iter().flatten()
            .map(|&sent| stop_event(stamp, ship, sent)).collect();
        if events.is_empty() { return Ok(Outcome::Stopped { count: 0 }) }
        let count = events.len() as i64;
        self.gm_previews.remove(&ship);
        if !self.observer_token(client).is_some_and(|token| self.emit_destiny(DestinyAudience::Observer(token), events)) { return Err(Error::SceneNotReady) }
        Ok(Outcome::Stopped { count })
    }
    fn gm_effect_slot_clear(&mut self, ship: i64, slot: usize) {
        if let Some(rec) = self.gm_previews.get_mut(&ship) {
            rec.slots[slot] = None;
            if rec.slots.iter().all(Option::is_none) { self.gm_previews.remove(&ship); }
        }
    }
    /// Stop every recorded preview of `ship` on the observer that owns the
    /// record, then drop it: the source ball is leaving or another caller is
    /// taking over the ship's preview state.
    fn gm_effect_release(&mut self, ship: i64) {
        let Some(rec) = self.gm_previews.remove(&ship) else { return };
        let stamp = self.current_time;
        let stops: Vec<Value> = rec.slots.iter().flatten()
            .map(|&sent| stop_event(stamp, ship, sent)).collect();
        if !stops.is_empty() {
            if let Some(token) = self.observer_token(rec.client) { self.emit_destiny(DestinyAudience::Observer(token), stops); }
        }
    }
    /// Drop the preview handles that reference `id` as source or target while
    /// the ball still exists, sending each recorded stop to its owning
    /// observer. Called from `take`, the shared leave/remove/replace point.
    pub(super) fn gm_effect_ball_leaving(&mut self, id: i64) {
        if self.gm_previews.is_empty() { return; }
        self.gm_effect_release(id);
        let mut stale = Vec::new();
        for (&source, rec) in &self.gm_previews {
            for sent in rec.slots.iter().flatten() {
                if sent.target == Some(id) { stale.push((source, sent.effect as usize)); }
            }
        }
        let stamp = self.current_time;
        for (source, slot) in stale {
            let Some((owner, sent)) = self.gm_previews.get_mut(&source).and_then(|rec| {
                let owner = rec.client;
                rec.slots[slot].take().map(|sent| (owner, sent))
            }) else { continue };
            if let Some(token) = self.observer_token(owner) {
                self.emit_destiny(DestinyAudience::Observer(token), vec![stop_event(stamp, source, sent)]);
            }
            if self.gm_previews.get(&source).is_some_and(|rec| rec.slots.iter().all(Option::is_none)) {
                self.gm_previews.remove(&source);
            }
        }
    }
    /// Stop this observer's previews that reference balls just removed from
    /// its scene, on the observer's own channel; handles stay bound to the
    /// client and character that sent them and no other observer's records.
    pub(super) fn gm_effect_scene_lost(&mut self, observer: ObserverToken, lost: &[BallToken]) {
        if self.gm_previews.is_empty() || lost.is_empty()
            || self.observers.get(&observer.client).is_none_or(|o| o.epoch != observer.epoch) { return; }
        let lost: HashSet<i64> = lost.iter().map(|token| token.id()).collect();
        let stamp = self.current_time;
        let mut events = Vec::new();
        let mut empty = Vec::new();
        for (&source, rec) in self.gm_previews.iter_mut() {
            if rec.client != observer.client { continue; }
            let whole = lost.contains(&source);
            for slot in rec.slots.iter_mut() {
                if !(whole || slot.as_ref().is_some_and(|s| s.target.is_some_and(|t| lost.contains(&t)))) { continue; }
                if let Some(sent) = slot.take() { events.push(stop_event(stamp, source, sent)); }
            }
            if rec.slots.iter().all(Option::is_none) { empty.push(source); }
        }
        for source in empty { self.gm_previews.remove(&source); }
        if !events.is_empty() { self.emit_destiny(DestinyAudience::Observer(observer), events); }
    }
    /// Drop every handle owned by `client` without sending stops: its
    /// observer is gone or is being replaced and never replays past previews.
    /// This cold path scans the recorded sources only.
    pub(super) fn gm_effect_observer_reset(&mut self, client: i64) {
        self.gm_previews.retain(|_, rec| rec.client != client);
    }
}
