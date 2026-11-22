//! Actor-owned transient inventory for NPC wrecks and the loose-loot cargo
//! containers a successful salvage leaves behind. Roots and their plain loot
//! live only in this actor: nothing here produces `game.items` rows, checkpoint
//! snapshots, sleep state or shutdown flushes, and a clean unload drops the
//! whole state with the actor. One claim per root serializes every
//! content-changing operation; its frozen plan, source receipt and reserved
//! IDs survive commit-uncertainty so a later exact proof finishes only the
//! actor delta without re-running business work. New item IDs come from
//! committed `game.items_item_id_seq` block reservations prepared by cold
//! loading and one bounded deferred worker, never synchronously per kill.

use super::*;
use super::observers::BallToken;
use crate::dao::items::loading::{GameItem, Item};
use crate::dao::items::quantities::QuantityChange;
use crate::db::outbox::SourceKey;
use crate::lua::events::GameEvent;
use crate::services::combat::groups_health::wreck::{debris_ball, PreparedNpcWreck};
use crate::services::identity::Identity;
use crate::services::inventory::reachability::root_in_range;
use crate::services::items::enrich_item;
use crate::services::CallError;
use crate::space::npc_wrecks::NpcWreckGuard;
use infinity_core::consts::GROUP_CARGO_CONTAINER;
use infinity_core::messages;
use std::collections::HashMap;
use std::sync::Arc;

mod mutations;

/// Root ID plus the maximum ordinary draw of two loot stacks; a reservation
/// must always fit one natural generation's complete demand.
const MAX_GENERATED_IDS: i64 = 3;

/// Outcome of one transient-root lookup. `Absent` means only that this actor
/// never owned or already removed the ID; `Denied` and `Busy` never fall back
/// to a database read.
pub(crate) enum NpcWreckLookup<T> {
    Absent,
    Denied,
    Busy(Option<Arc<NpcWreckClaim>>),
    Ready(T),
}

/// The two transient roots this state produces.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NpcWreckKind { Wreck, LooseLootContainer }

/// Same-turn admission snapshot binding a root to this actor incarnation, its
/// live ball incarnation and one content revision. Stale identities are denied.
#[derive(Clone)]
pub(crate) struct NpcWreckIdentity {
    incarnation: Arc<()>,
    token: BallToken,
    root: i64,
    revision: i64,
}

/// One consistent root+children read captured inside a single actor turn.
/// The live ball is admitted and not retained.
#[derive(Clone)]
pub(crate) struct NpcWreckView {
    pub(crate) identity: NpcWreckIdentity,
    pub(crate) kind: NpcWreckKind,
    pub(crate) root: Item,
    pub(crate) items: Vec<Item>,
    pub(crate) name: Option<String>,
}

/// One located loot child together with its containing root snapshot.
pub(crate) struct NpcLootSource {
    pub(crate) wreck: NpcWreckView,
    pub(crate) item: Item,
}

/// One recovery attempt's activation identity: a module cycle's real stop
/// generation (`Arc` identity, not its current boolean) or a drone order's
/// generation. A parked attempt cannot be impersonated by a fresh `Arc` or a
/// same-value boolean.
#[derive(Clone)]
pub(crate) enum RecoveryAttempt {
    Module { master: i64, stop: Arc<std::sync::atomic::AtomicBool> },
    Drone { item: i64, generation: u64 },
}
impl RecoveryAttempt {
    pub(crate) fn same_attempt(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Module { master: a, stop: x }, Self::Module { master: b, stop: y }) =>
                a == b && Arc::ptr_eq(x, y),
            (Self::Drone { item: a, generation: x }, Self::Drone { item: b, generation: y }) =>
                a == b && x == y,
            _ => false,
        }
    }
}

/// Who asks for a claim: an inventory-bound pilot, a recovery cycle proving
/// its source ball and operational range, or GM cleanup.
#[derive(Clone)]
pub(crate) enum NpcWreckAccess {
    Inventory(Identity),
    Recovery { character: i64, ship: i64, source: i64, source_token: BallToken, range: f64,
        attempt: RecoveryAttempt },
    GmCleanup { character: i64 },
}

/// The complete frozen plan of one write attempt.
#[derive(Clone)]
pub(crate) enum NpcWreckOperation {
    Take { item: i64, quantity: i64, character: i64, destination: i64 },
    Salvage { character: i64, destination: i64, yields: Vec<(i32, i64)>, event: GameEvent },
    /// `items` naming the root ID deletes root and all children; otherwise it
    /// names plain children. A mixed cleanup also freezes the persistent
    /// preimage its own proof must re-verify: `persistent_roots` are the
    /// persistent root IDs and `persistent` the complete pre-delete rows.
    Delete { items: Vec<i64>, persistent_roots: Vec<i64>, persistent: Vec<Item> },
}

/// One immutable root claim: frozen operation, reserved IDs, taken slice,
/// replacement ball and commit receipt. Actors and outside transactions share
/// it through `Arc`; nothing here is mutable after publication.
pub(crate) struct NpcWreckClaim {
    system: i64,
    root: i64,
    serial: i64,
    expected: NpcWreckIdentity,
    access: NpcWreckAccess,
    operation: NpcWreckOperation,
    taken: Option<Item>,
    replacement: Option<Ball>,
    receipt: Option<NpcWreckReceipt>,
    prepared: bool,
}

impl NpcWreckClaim {
    pub(crate) fn system(&self) -> i64 { self.system }
    pub(crate) fn root(&self) -> i64 { self.root }
    pub(crate) fn serial(&self) -> i64 { self.serial }
    pub(crate) fn operation(&self) -> &NpcWreckOperation { &self.operation }
    /// The complete frozen access (including the source ball token and the
    /// recovery attempt identity); callers compare, never mutate.
    pub(crate) fn access(&self) -> &NpcWreckAccess { &self.access }
    /// The only taken slice: whole original ID or a reserved partial ID with
    /// the exact quantity, still carrying the source owner until `move_core`.
    pub(crate) fn taken(&self) -> Option<&Item> { self.taken.as_ref() }
    pub(crate) fn receipt(&self) -> Option<&NpcWreckReceipt> { self.receipt.as_ref() }
    /// Every required ID is bound; a `false` claim is a parked WaitingIds plan.
    pub(crate) fn prepared(&self) -> bool { self.prepared }
    /// Same frozen attempt: same actor incarnation, root, ball token,
    /// revision and operation serial, regardless of the `Arc` identity.
    fn same_attempt(&self, other: &NpcWreckClaim) -> bool {
        self.serial == other.serial && self.root == other.root
            && self.expected.revision == other.expected.revision
            && self.expected.token == other.expected.token
            && Arc::ptr_eq(&self.expected.incarnation, &other.expected.incarnation)
    }
}

/// Canonical commit receipt: the exact progress source key and the complete
/// expected event the operation's transaction must have journaled.
pub(crate) struct NpcWreckReceipt {
    kind: &'static str,
    identity: String,
    ordinal: i64,
    event: GameEvent,
}

impl NpcWreckReceipt {
    fn new(kind: &'static str, identity: String, event: GameEvent) -> Self {
        Self { kind, identity, ordinal: 0, event }
    }
    pub(crate) fn source_key(&self) -> SourceKey { SourceKey::canonical(self.kind, &self.identity, self.ordinal) }
    pub(crate) fn event(&self) -> &GameEvent { &self.event }
}

/// What an exact proof established about one claim's transaction.
#[derive(Clone, Copy)]
pub(crate) enum NpcWreckDisposition { Committed, RolledBack, Unknown }

/// A pure in-actor content change between turns; every variant verifies the
/// original owner rules before mutating.
pub(crate) enum NpcLootMutation {
    Split { item: i64, quantity: i64 },
    Quantities(std::collections::BTreeMap<i64, QuantityChange>),
    Move { item: i64, quantity: i64, destination: NpcWreckIdentity, flag: i32 },
    Rename { name: String, row: marshal::Value, caller: i64 },
}

/// Lifecycle of the single claim a root admits at a time. `WaitingIds` is a
/// parked never-executed plan; `Unknown` keeps the root sealed until proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NpcWreckClaimState { WaitingIds, Claimed, Unknown }

struct ClaimSlot {
    claim: Arc<NpcWreckClaim>,
    status: NpcWreckClaimState,
}

/// One transient root: its inventory identity, plain loot ordered by item ID
/// and the claim slot. `ball_departed` keeps a claim's tombstone sealed after
/// its ball left the scene until `finish` proves the disposition.
struct NpcWreckRoot {
    kind: NpcWreckKind,
    token: BallToken,
    item: Item,
    items: Vec<Item>,
    name: Option<String>,
    revision: i64,
    claim: Option<ClaimSlot>,
    ball_departed: bool,
}

/// All transient roots, the child index and the two-block reserved ID pool.
/// The `Arc<()>` incarnation distinguishes this exact actor instance, so a
/// stale completion can never mutate a same-system successor.
#[derive(Default)]
pub(crate) struct NpcWrecks {
    roots: HashMap<i64, NpcWreckRoot>,
    child_to_root: HashMap<i64, i64>,
    incarnation: Arc<()>,
    ids: Vec<i64>,
    cursor: usize,
    next_ids: Option<Vec<i64>>,
    refill_in_flight: bool,
    refill_after: i64,
    refill_serial: u64,
    operation_serial: i64,
}

impl NpcWrecks {
    fn ids_available(&self) -> usize {
        self.ids.len() - self.cursor + self.next_ids.as_ref().map_or(0, Vec::len)
    }
    /// Draw `count` consecutive reserved IDs. A shortfall consumes nothing.
    fn draw_ids(&mut self, count: usize) -> Option<Vec<i64>> {
        if count == 0 { return Some(Vec::new()); }
        if self.ids_available() < count { return None; }
        let mut drawn = Vec::with_capacity(count);
        while drawn.len() < count {
            if self.cursor == self.ids.len() {
                let Some(next) = self.next_ids.take() else { return None };
                self.ids = next;
                self.cursor = 0;
                if self.ids.is_empty() { return None; }
            }
            drawn.push(self.ids[self.cursor]);
            self.cursor += 1;
        }
        Some(drawn)
    }
    /// Install one committed reservation: it becomes the current block when
    /// the old one is drained, otherwise the single next block.
    fn install_block(&mut self, ids: Vec<i64>) {
        if self.cursor == self.ids.len() { self.ids = ids; self.cursor = 0; }
        else { self.next_ids = Some(ids); }
    }
    fn next_serial(&mut self) -> Option<i64> {
        self.operation_serial = self.operation_serial.checked_add(1)?;
        Some(self.operation_serial)
    }
    fn drop_root(&mut self, root: i64) {
        if let Some(entry) = self.roots.remove(&root) {
            for item in entry.items { self.child_to_root.remove(&item.item_id); }
        }
    }
}

/// The access characters must match the operation's frozen actor. An
/// inventory-bound delete may name the root itself only when that pilot owns
/// the root (TrashItems of an own loose-loot container); every named child
/// keeps the original owner rule.
fn authority_matches(access: &NpcWreckAccess, operation: &NpcWreckOperation, entry: &NpcWreckRoot) -> bool {
    match (access, operation) {
        (NpcWreckAccess::Inventory(identity), NpcWreckOperation::Take { character, .. }) => identity.char_id == *character,
        (NpcWreckAccess::Inventory(identity), NpcWreckOperation::Delete { items, .. }) =>
            !items.is_empty() && items.iter().all(|&id|
                (id == entry.item.item_id && entry.item.owner_id == identity.char_id)
                    || entry.items.iter().any(|item| item.item_id == id && item.owner_id == identity.char_id)),
        (NpcWreckAccess::Recovery { character, .. }, NpcWreckOperation::Salvage { character: expected, .. }) => character == expected,
        (NpcWreckAccess::GmCleanup { .. }, NpcWreckOperation::Delete { .. }) => true,
        _ => false,
    }
}

/// The parked attempt's stored authority must equal the resuming request's;
/// `Identity` has no `PartialEq`, so fields compare explicitly.
fn same_authority(stored: &NpcWreckAccess, access: &NpcWreckAccess) -> bool {
    match (stored, access) {
        (NpcWreckAccess::Inventory(a), NpcWreckAccess::Inventory(b)) =>
            a.char_id == b.char_id && a.ship_id == b.ship_id && a.corp_id == b.corp_id
                && a.station_id == b.station_id && a.system_id == b.system_id,
        (NpcWreckAccess::Recovery { character: a1, ship: a2, source: a3, source_token: a4, range: a5, attempt: a6 },
            NpcWreckAccess::Recovery { character: b1, ship: b2, source: b3, source_token: b4, range: b5, attempt: b6 }) =>
            a1 == b1 && a2 == b2 && a3 == b3 && a4 == b4 && a5 == b5 && a6.same_attempt(b6),
        (NpcWreckAccess::GmCleanup { character: a }, NpcWreckAccess::GmCleanup { character: b }) => a == b,
        _ => false,
    }
}

fn busy() -> CallError { crate::services::notify_error(messages::fitting::CHARGES_SETTLING) }
fn denied() -> CallError { crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }

impl System {
    /// Real pilot ball, ship/session match, live visibility and the sampled
    /// surface gap evaluated by the shared `root_in_range` policy.
    fn npc_wreck_admit(&self, identity: &Identity, entry: &NpcWreckRoot) -> bool {
        if identity.station_id.is_some() || identity.system_id != Some(self.id) { return false; }
        let Some(ego) = self.pilot_ball(identity.char_id) else { return false };
        if identity.ship_id != Some(ego.id) { return false; }
        let Some(ball) = self.ball(entry.item.item_id) else { return false };
        if !self.visible_to(identity.char_id, entry.item.item_id) { return false; }
        let Some(sde) = self.sde() else { return false; };
        let gap = dist(ego.position_at(self.ms), ball.position_at(self.ms))
            - f64::from(ego.radius) - f64::from(ball.radius);
        root_in_range(sde, entry.item.type_id, entry.item.flag_id, false, Some(gap))
    }

    /// Per-access admission: inventory runs the full pilot/range gate,
    /// recovery proves its own source ball incarnation and module/drone range,
    /// and GM cleanup trusts the upstream role check.
    fn npc_wreck_access_admit(&self, access: &NpcWreckAccess, entry: &NpcWreckRoot) -> bool {
        match access {
            NpcWreckAccess::Inventory(identity) => self.npc_wreck_admit(identity, entry),
            NpcWreckAccess::Recovery { character, ship, source, source_token, range, .. } => {
                if !self.ball_token_current(*source_token) || !range.is_finite() || *range < 0.0 { return false; }
                let (Some(source_ball), Some(root_ball)) = (self.ball(*source), self.ball(entry.item.item_id)) else { return false; };
                if self.pilot_ball(*character).is_none_or(|pilot| pilot.id != *ship) { return false; }
                let gap = dist(source_ball.position_at(self.ms), root_ball.position_at(self.ms))
                    - f64::from(source_ball.radius) - f64::from(root_ball.radius);
                gap.is_finite() && gap <= *range
            }
            NpcWreckAccess::GmCleanup { .. } => true,
        }
    }

    /// Pilots currently able to see the root inside its operational range;
    /// captured at the final mutation turn, never maintained as a subscription.
    fn npc_wreck_audience(&self, root: i64, type_id: i32, flag: i32) -> Vec<i64> {
        let (Some(ball), Some(sde)) = (self.ball(root), self.sde()) else { return Vec::new() };
        let root_pos = ball.position_at(self.ms);
        self.pilots.keys().copied().filter(|&character| {
            let Some(ego) = self.pilot_ball(character) else { return false };
            if !self.visible_to(character, root) { return false; }
            let gap = dist(ego.position_at(self.ms), root_pos)
                - f64::from(ego.radius) - f64::from(ball.radius);
            root_in_range(sde, type_id, flag, false, Some(gap))
        }).collect()
    }

    /// Original `OnItemChange` source deltas to the captured live audience.
    fn npc_wreck_emit(&self, root: i64, type_id: i32, flag: i32, payloads: Vec<(Value, Value)>) {
        if payloads.is_empty() { return; }
        for character in self.npc_wreck_audience(root, type_id, flag) {
            for (row, change) in &payloads {
                self.notify_pilot(character, "OnItemChange", vec![row.clone(), change.clone(), Value::None]);
            }
        }
    }

    /// Publish `isEmpty` through the ordered command/slim channel, not by
    /// silently editing the slim field.
    fn npc_wreck_set_is_empty(&mut self, root: i64, empty: bool) {
        self.command(root, move |ball, stamp, _| {
            ball.slim.retain(|(key, _)| *key != "isEmpty");
            ball.slim.push(("isEmpty", Value::Bool(empty)));
            vec![event(stamp, "OnSlimItemChange", vec![Value::Int(root), ball.slim_item()])]
        });
    }
    /// Update the slim only on a real empty-state transition.
    fn npc_wreck_refresh_is_empty(&mut self, root: i64, was_empty: bool) {
        let empty = self.npc_wrecks.roots.get(&root).is_none_or(|entry| entry.items.is_empty());
        if empty != was_empty { self.npc_wreck_set_is_empty(root, empty); }
    }

    fn npc_wreck_view_of(&self, root: i64) -> Option<NpcWreckView> {
        let entry = self.npc_wrecks.roots.get(&root)?;
        self.ball(root)?;
        Some(NpcWreckView {
            identity: NpcWreckIdentity {
                incarnation: self.npc_wrecks.incarnation.clone(),
                token: entry.token, root, revision: entry.revision,
            },
            kind: entry.kind,
            root: entry.item.clone(),
            items: entry.items.clone(),
            name: entry.name.clone(),
        })
    }

    /// Enrich one in-memory row; an absent SDE keeps zero group/category like
    /// an unloaded type, which can only hide a row, never fabricate one.
    fn npc_wreck_enrich(&self, row: GameItem) -> Item {
        let GameItem { item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, mut custom_info } = row;
        self.sde().filter(|sde| sde.types.get(type_id).is_some())
            .and_then(|sde| enrich_item(sde, GameItem { item_id, type_id, owner_id, location_id, flag_id,
                quantity, singleton, custom_info: custom_info.take() }))
            .unwrap_or_else(|| Item { item_id, type_id, owner_id, location_id, flag_id, quantity, singleton,
                group_id: 0, category_id: 0, custom_info })
    }

    /// Install root ball first, then capture its incarnation token, then
    /// register state: `insert` can no longer replace it unseen.
    fn install_npc_root(&mut self, kind: NpcWreckKind, ball: Ball, items: Vec<Item>, name: Option<String>) {
        let root = ball.id;
        let item = self.npc_wreck_enrich(GameItem {
            item_id: root, type_id: ball.type_id, owner_id: ball.owner_id,
            location_id: self.id, flag_id: 0, quantity: 1, singleton: true, custom_info: name.clone(),
        });
        self.add_ship(ball);
        let Some(token) = self.capture_ball(root) else { return };
        for child in &items { self.npc_wrecks.child_to_root.insert(child.item_id, root); }
        self.npc_wrecks.roots.insert(root, NpcWreckRoot {
            kind, token, item, items, name, revision: 0, claim: None, ball_departed: false,
        });
    }

    /// Cold-loaded reservation; committed blocks only, installed before the
    /// actor starts.
    pub(crate) fn npc_wreck_install_ids(&mut self, ids: Vec<i64>) {
        self.npc_wrecks.install_block(ids);
    }

    /// Delivery from the deferred reserve worker; a stale serial, stale
    /// incarnation or dead channel discards the block instead of mutating.
    pub(crate) fn npc_wreck_refill_delivered(&mut self, incarnation: Arc<()>, serial: u64, block: Option<Vec<i64>>) {
        if !Arc::ptr_eq(&incarnation, &self.npc_wrecks.incarnation) { return; }
        if !self.npc_wrecks.refill_in_flight || self.npc_wrecks.refill_serial != serial { return; }
        self.npc_wrecks.refill_in_flight = false;
        match block {
            Some(ids) if !ids.is_empty() => self.npc_wrecks.install_block(ids),
            _ => {
                let retry = crate::config::game().combat.npc_wreck_id_retry_ms;
                self.npc_wrecks.refill_after = self.ms.saturating_add(retry);
            }
        }
    }

    /// One bounded refill attempt when the pool drops below the named low
    /// water. Only real demand reaches here; there is no self-renewing timer.
    fn npc_wreck_ensure_ids(&mut self) {
        if self.world.upgrade().is_none() { return; }
        let config = &crate::config::game().combat;
        let low = (config.npc_wreck_id_block_size / 4).max(MAX_GENERATED_IDS);
        if self.npc_wrecks.refill_in_flight
            || i64::try_from(self.npc_wrecks.ids_available()).unwrap_or(i64::MAX) >= low
            || self.ms < self.npc_wrecks.refill_after { return; }
        self.npc_wrecks.refill_in_flight = true;
        self.npc_wrecks.refill_serial = self.npc_wrecks.refill_serial.checked_add(1)
            .expect("npc wreck refill serial exhausted");
        let job = crate::space::npc_wrecks::refill_ids(self.world.clone(), self.id,
            self.npc_wrecks.incarnation.clone(), self.npc_wrecks.refill_serial);
        if !self.try_defer(job) { self.npc_wrecks.refill_in_flight = false; }
    }


    /// Bind a frozen draft to reserved root+stack IDs once. `false` leaves
    /// the draft untouched for the next finite retry after refill.
    pub(crate) fn npc_wreck_bind_prepared(&mut self, prepared: Option<&PreparedNpcWreck>) -> bool {
        let Some(prepared) = prepared else { return true };
        if prepared.ids.get().is_some() { return true; }
        let Some(ids) = self.npc_wrecks.draw_ids(1 + prepared.loot.len()) else {
            self.npc_wreck_ensure_ids();
            return false;
        };
        prepared.ids.set(ids).is_ok()
    }

    /// Publish one prepared natural-death wreck inside the finalize turn.
    /// A missing binding publishes nothing instead of fabricating IDs.
    pub(crate) fn install_npc_wreck(&mut self, prepared: &PreparedNpcWreck) -> bool {
        let Some(ids) = prepared.ids.get() else { return false };
        if ids.len() != 1 + prepared.loot.len() { return false; }
        let root = ids[0];
        let items: Vec<Item> = prepared.loot.iter().zip(ids[1..].iter()).map(|(&(type_id, quantity), &id)| {
            self.npc_wreck_enrich(GameItem {
                item_id: id, type_id, owner_id: prepared.owner, location_id: root,
                flag_id: 0, quantity, singleton: false, custom_info: None,
            })
        }).collect();
        let empty = items.is_empty();
        let ball = debris_ball(root, prepared.wreck_type, prepared.owner, prepared.corp,
            prepared.pos, prepared.radius, Some(prepared.source_ship), empty, self.ms);
        self.install_npc_root(NpcWreckKind::Wreck, ball, items, None);
        self.npc_wreck_ensure_ids();
        true
    }

    /// GM `/wreck`: draw a complete ID set and install an empty/looted root
    /// in this same actor turn; a shortfall refuses the whole spawn.
    pub(crate) fn npc_wreck_spawn(&mut self, wreck_type: i32, owner: i64, corp: i64,
        pos: V3, radius: f32, source_ship: Option<i64>, loot: Vec<(i32, i64)>) -> Option<i64> {
        let Some(ids) = self.npc_wrecks.draw_ids(1 + loot.len()) else {
            self.npc_wreck_ensure_ids();
            return None;
        };
        let root = ids[0];
        let items: Vec<Item> = loot.iter().zip(ids[1..].iter()).map(|(&(type_id, quantity), &id)| {
            self.npc_wreck_enrich(GameItem {
                item_id: id, type_id, owner_id: owner, location_id: root,
                flag_id: 0, quantity, singleton: false, custom_info: None,
            })
        }).collect();
        let empty = items.is_empty();
        let ball = debris_ball(root, wreck_type, owner, corp, pos, radius, source_ship, empty, self.ms);
        self.install_npc_root(NpcWreckKind::Wreck, ball, items, None);
        self.npc_wreck_ensure_ids();
        Some(root)
    }

    /// State check first, then the claim gate, then identity: an ordinary
    /// database ID can never be mistaken for a transient root.
    pub(crate) fn npc_wreck_view(&mut self, identity: &Identity, root: i64) -> NpcWreckLookup<NpcWreckView> {
        let Some(entry) = self.npc_wrecks.roots.get(&root) else { return NpcWreckLookup::Absent };
        if let Some(slot) = &entry.claim { return NpcWreckLookup::Busy(Some(slot.claim.clone())); }
        if entry.ball_departed { return NpcWreckLookup::Absent; }
        if !self.npc_wreck_admit(identity, entry) { return NpcWreckLookup::Denied; }
        match self.npc_wreck_view_of(root) {
            Some(view) => NpcWreckLookup::Ready(view),
            None => NpcWreckLookup::Absent,
        }
    }

    /// Locate one plain child through the reverse index; `source` names the
    /// root the caller believes it sits in.
    pub(crate) fn npc_loot_source(&mut self, identity: &Identity, item: i64, source: Option<i64>) -> NpcWreckLookup<NpcLootSource> {
        let Some(&root) = self.npc_wrecks.child_to_root.get(&item) else { return NpcWreckLookup::Absent };
        if source.is_some_and(|source| source != root) { return NpcWreckLookup::Denied; }
        let Some(entry) = self.npc_wrecks.roots.get(&root) else { return NpcWreckLookup::Absent };
        if let Some(slot) = &entry.claim { return NpcWreckLookup::Busy(Some(slot.claim.clone())); }
        if entry.ball_departed { return NpcWreckLookup::Absent; }
        if !self.npc_wreck_admit(identity, entry) { return NpcWreckLookup::Denied; }
        let (Some(view), Some(item)) = (self.npc_wreck_view_of(root),
            self.npc_wrecks.roots.get(&root).and_then(|entry| entry.items.iter().find(|i| i.item_id == item).cloned()))
        else { return NpcWreckLookup::Absent };
        NpcWreckLookup::Ready(NpcLootSource { wreck: view, item })
    }

    /// Recovery-side read: the access itself proves range and source, so the
    /// inventory 2.5 km rule is never substituted for module/drone range.
    pub(crate) fn npc_wreck_recovery_view(&mut self, access: &NpcWreckAccess, root: i64) -> NpcWreckLookup<NpcWreckView> {
        let Some(entry) = self.npc_wrecks.roots.get(&root) else { return NpcWreckLookup::Absent };
        if let Some(slot) = &entry.claim { return NpcWreckLookup::Busy(Some(slot.claim.clone())); }
        if entry.ball_departed { return NpcWreckLookup::Absent; }
        if !self.npc_wreck_access_admit(access, entry) { return NpcWreckLookup::Denied; }
        match self.npc_wreck_view_of(root) {
            Some(view) => NpcWreckLookup::Ready(view),
            None => NpcWreckLookup::Absent,
        }
    }

    /// Claim one root for one frozen plan. The guard is constructed here
    /// inside the claiming actor turn, so a cancelled caller can never strand
    /// a claim without a Drop disposition.
    pub(crate) fn claim_npc_wreck(&mut self, expected: &NpcWreckIdentity, access: NpcWreckAccess,
        operation: NpcWreckOperation) -> NpcWreckLookup<NpcWreckGuard> {
        let root = expected.root;
        {
            let Some(entry) = self.npc_wrecks.roots.get(&root) else { return NpcWreckLookup::Absent };
            if let Some(slot) = &entry.claim { return NpcWreckLookup::Busy(Some(slot.claim.clone())); }
            if entry.ball_departed { return NpcWreckLookup::Absent; }
            if expected.revision != entry.revision || expected.token != entry.token
                || !Arc::ptr_eq(&expected.incarnation, &self.npc_wrecks.incarnation)
                || !self.ball_token_current(expected.token) { return NpcWreckLookup::Denied; }
            if !self.npc_wreck_access_admit(&access, entry) { return NpcWreckLookup::Denied; }
            if !authority_matches(&access, &operation, entry) { return NpcWreckLookup::Denied; }
        }
        let Some(serial) = self.npc_wrecks.next_serial() else { return NpcWreckLookup::Busy(None) };
        let (taken, replacement, receipt, prepared) = match &operation {
            NpcWreckOperation::Take { item, quantity, character, destination, .. } => {
                let Some(source_item) = self.npc_wrecks.roots.get(&root)
                    .and_then(|entry| entry.items.iter().find(|i| i.item_id == *item).cloned())
                else { return NpcWreckLookup::Denied };
                if *quantity <= 0 || *quantity > source_item.quantity { return NpcWreckLookup::Denied; }
                let taken = if *quantity == source_item.quantity { source_item } else {
                    let Some(ids) = self.npc_wrecks.draw_ids(1) else {
                        self.npc_wreck_ensure_ids();
                        return NpcWreckLookup::Busy(None);
                    };
                    Item { item_id: ids[0], quantity: *quantity, singleton: false, ..source_item }
                };
                let receipt = NpcWreckReceipt::new("NpcLootTaken",
                    format!("{}/{}/{}/{}", root, item, expected.revision, serial),
                    GameEvent::ItemsMoved {
                        char_id: *character, item_id: taken.item_id,
                        from_location_id: root, to_location_id: *destination, quantity: *quantity,
                    });
                (Some(taken), None, Some(receipt), true)
            }
            NpcWreckOperation::Salvage { event, .. } => {
                let valid = matches!(event, GameEvent::WreckSalvaged { wreck_id, .. } if *wreck_id == root);
                if !valid { return NpcWreckLookup::Denied; }
                let receipt = NpcWreckReceipt::new("WreckSalvaged", root.to_string(), event.clone());
                let has_items = self.npc_wrecks.roots.get(&root).is_some_and(|entry| !entry.items.is_empty());
                if !has_items { (None, None, Some(receipt), true) } else {
                    match self.npc_wrecks.draw_ids(1) {
                        Some(ids) => match self.npc_wreck_replacement_ball(root, ids[0]) {
                            Some(ball) => (None, Some(ball), Some(receipt), true),
                            // No SDE container type: a defined, never-executed
                            // refusal the caller treats as unavailable.
                            None => return NpcWreckLookup::Denied,
                        },
                        // Keep the frozen draw waiting for refill instead of
                        // re-rolling yields on the next attempt.
                        None => (None, None, Some(receipt), false),
                    }
                }
            }
            NpcWreckOperation::Delete { items, .. } => {
                let valid = !items.is_empty() && items.iter().all(|&id| id == root
                    || self.npc_wrecks.roots.get(&root).is_some_and(|entry| entry.items.iter().any(|i| i.item_id == id)));
                if !valid { return NpcWreckLookup::Denied; }
                (None, None, None, true)
            }
        };
        let claim = Arc::new(NpcWreckClaim {
            system: self.id, root, serial,
            expected: expected.clone(), access, operation, taken, replacement, receipt, prepared,
        });
        let status = if prepared { NpcWreckClaimState::Claimed } else { NpcWreckClaimState::WaitingIds };
        if let Some(entry) = self.npc_wrecks.roots.get_mut(&root) {
            entry.claim = Some(ClaimSlot { claim: claim.clone(), status });
        }
        self.npc_wreck_ensure_ids();
        NpcWreckLookup::Ready(NpcWreckGuard::new(self.world.clone(), claim))
    }

    /// Resume a parked never-executed WaitingIds attempt after refill: the
    /// stored authority must equal the new request's, the frozen source must
    /// still hold, and only then are the missing IDs bound into a new claim.
    /// Claimed/Unknown roots never get a second writer.
    pub(crate) fn resume_npc_wreck(&mut self, claim: &Arc<NpcWreckClaim>, access: NpcWreckAccess) -> NpcWreckLookup<NpcWreckGuard> {
        let root = claim.root;
        {
            let Some(entry) = self.npc_wrecks.roots.get(&root) else { return NpcWreckLookup::Absent };
            let Some(slot) = &entry.claim else { return NpcWreckLookup::Absent };
            if !slot.claim.same_attempt(claim) { return NpcWreckLookup::Busy(Some(slot.claim.clone())); }
            if !matches!(slot.status, NpcWreckClaimState::WaitingIds) { return NpcWreckLookup::Busy(Some(slot.claim.clone())); }
            if !same_authority(&slot.claim.access, &access) { return NpcWreckLookup::Denied; }
            if entry.ball_departed || !self.ball_token_current(claim.expected.token) { return NpcWreckLookup::Absent; }
            if !self.npc_wreck_access_admit(&access, entry) { return NpcWreckLookup::Denied; }
        }
        let replacement = match &claim.operation {
            NpcWreckOperation::Salvage { .. }
                if self.npc_wrecks.roots.get(&root).is_some_and(|entry| !entry.items.is_empty()) => {
                let Some(ids) = self.npc_wrecks.draw_ids(1) else {
                    self.npc_wreck_ensure_ids();
                    return NpcWreckLookup::Busy(Some(claim.clone()));
                };
                match self.npc_wreck_replacement_ball(root, ids[0]) {
                    Some(ball) => Some(ball),
                    None => return NpcWreckLookup::Denied,
                }
            }
            _ => claim.replacement.clone(),
        };
        let completed = Arc::new(NpcWreckClaim {
            system: claim.system, root, serial: claim.serial,
            expected: claim.expected.clone(), access: access.clone(),
            operation: claim.operation.clone(), taken: claim.taken.clone(),
            replacement, receipt: match claim.receipt.as_ref() {
                Some(receipt) => Some(NpcWreckReceipt {
                    kind: receipt.kind, identity: receipt.identity.clone(),
                    ordinal: receipt.ordinal, event: receipt.event.clone(),
                }),
                None => None,
            },
            prepared: true,
        });
        if let Some(entry) = self.npc_wrecks.roots.get_mut(&root) {
            if let Some(slot) = &mut entry.claim {
                slot.claim = completed.clone();
                slot.status = NpcWreckClaimState::Claimed;
            }
        }
        self.npc_wreck_ensure_ids();
        NpcWreckLookup::Ready(NpcWreckGuard::new(self.world.clone(), completed))
    }

    /// The replacement loose-loot container: smallest SDE cargo-container
    /// type at the claim-time sampled position (a tractor may have moved the
    /// wreck), the original recovery radius rule, no launcher link and no
    /// inherited name.
    fn npc_wreck_replacement_ball(&self, root: i64, id: i64) -> Option<Ball> {
        let sde = self.sde()?;
        let kind = sde.types.iter().filter(|t| t.group_id == Some(GROUP_CARGO_CONTAINER))
            .min_by_key(|t| t.type_id)?.type_id;
        let entry = self.npc_wrecks.roots.get(&root)?;
        let ball = self.sampled_ball(root)?;
        let radius = sde.types.get(kind).and_then(|t| t.radius).map(|radius| radius as f32).unwrap_or(ball.radius);
        Some(debris_ball(id, kind, entry.item.owner_id, ball.corp_id, ball.pos, radius, None, false, self.ms))
    }

    /// Settle one claim: Committed applies its frozen delta exactly once,
    /// RolledBack restores the unchanged list, Unknown keeps the root sealed.
    /// A mismatched epoch/serial/generation has no effect.
    pub(crate) fn finish_npc_wreck(&mut self, claim: &Arc<NpcWreckClaim>, outcome: NpcWreckDisposition) -> bool {
        let root = claim.root;
        {
            let Some(entry) = self.npc_wrecks.roots.get(&root) else { return false };
            let Some(slot) = &entry.claim else { return false };
            if !slot.claim.same_attempt(claim) { return false; }
        }
        match outcome {
            NpcWreckDisposition::Unknown => {
                if let Some(slot) = self.npc_wrecks.roots.get_mut(&root).and_then(|entry| entry.claim.as_mut()) {
                    slot.status = NpcWreckClaimState::Unknown;
                }
                true
            }
            NpcWreckDisposition::RolledBack => {
                let departed = self.npc_wrecks.roots.get(&root).is_some_and(|entry| entry.ball_departed);
                if let Some(entry) = self.npc_wrecks.roots.get_mut(&root) { entry.claim = None; }
                if departed { self.npc_wrecks.drop_root(root); }
                true
            }
            NpcWreckDisposition::Committed => {
                // Apply the stored claim: a caller may hold an older Arc of the
                // same frozen attempt (for example the unparked preimage), so
                // the registered claim is authoritative.
                let Some(stored) = self.npc_wrecks.roots.get_mut(&root)
                    .and_then(|entry| entry.claim.take().map(|slot| slot.claim))
                else { return false };
                match &stored.operation {
                    NpcWreckOperation::Take { .. } => self.npc_wreck_apply_take(&stored),
                    NpcWreckOperation::Salvage { .. } => self.npc_wreck_apply_salvage(&stored),
                    NpcWreckOperation::Delete { items, .. } => self.npc_wreck_apply_delete(root, items),
                }
                true
            }
        }
    }

    /// Read-only status of the registered claim slot for this exact attempt:
    /// the root and a slot must exist, the passed Arc must name the same
    /// frozen attempt, and the stored incarnation, ball token and revision
    /// must still match the entry. A departed-ball tombstone keeps proving
    /// identity through its stored token.
    pub(crate) fn npc_wreck_claim_status(&self, claim: &Arc<NpcWreckClaim>) -> Option<NpcWreckClaimState> {
        let entry = self.npc_wrecks.roots.get(&claim.root)?;
        let slot = entry.claim.as_ref()?;
        if !slot.claim.same_attempt(claim)
            || !Arc::ptr_eq(&slot.claim.expected.incarnation, &self.npc_wrecks.incarnation)
            || slot.claim.expected.token != entry.token
            || slot.claim.expected.revision != entry.revision { return None; }
        Some(slot.status)
    }

    /// Terminate a never-executed WaitingIds attempt: only while the
    /// registered slot still holds the same unprepared claim may it roll
    /// back, so a stale Arc of an already-resumed attempt can never release
    /// the active writer.
    pub(crate) fn cancel_waiting_npc_wreck(&mut self, claim: &Arc<NpcWreckClaim>) -> bool {
        if self.npc_wreck_claim_status(claim) != Some(NpcWreckClaimState::WaitingIds) { return false; }
        if self.npc_wrecks.roots.get(&claim.root)
            .and_then(|entry| entry.claim.as_ref()).is_some_and(|slot| slot.claim.prepared()) { return false; }
        self.finish_npc_wreck(claim, NpcWreckDisposition::RolledBack)
    }

    /// Any root claim still sealed (Claimed, parked WaitingIds or Unknown)
    /// with or without its ball. Such an obligation never permits unload:
    /// a departed-ball tombstone is exactly the case the ball scan misses.
    pub(crate) fn npc_wreck_claims_pending(&self) -> bool {
        self.npc_wrecks.roots.values().any(|entry| entry.claim.is_some())
    }

    /// Unload exemption: only this exact root identity, ball token current,
    /// no claim and no tombstone. Anything else stays a dynamic-ball blocker.
    pub(crate) fn npc_wreck_discardable(&self, id: i64) -> bool {
        let Some(entry) = self.npc_wrecks.roots.get(&id) else { return false };
        entry.claim.is_none() && !entry.ball_departed && self.ball_token_current(entry.token)
    }

    /// A busy root can never be silently replaced by a same-ID insert.
    pub(crate) fn npc_wreck_blocks_insert(&self, id: i64) -> bool {
        self.npc_wrecks.roots.get(&id).is_some_and(|entry| entry.claim.is_some())
    }

    /// Ball departure: an unclaimed root drops its state entirely; a claimed
    /// one becomes a sealed tombstone until `finish` proves the outcome.
    pub(crate) fn npc_wreck_root_departed(&mut self, id: i64) {
        if self.npc_wrecks.roots.get(&id).is_some_and(|entry| entry.claim.is_some()) {
            if let Some(entry) = self.npc_wrecks.roots.get_mut(&id) { entry.ball_departed = true; }
        } else {
            self.npc_wrecks.drop_root(id);
        }
    }
}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
