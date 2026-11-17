//! Coarse grid topology: fixed-edge boxes, box occupancy and monotonic bubble
//! labels. Mirrors carbonengine destiny `Partition.cpp`'s coarsest cell: every
//! ball, cloaked and global alike, floors into a 7,864,320 m box per axis.
//! A newly occupied box adopts its own existing label, else the first labelled
//! box among its 26 neighbours in fixed order, else a fresh checked id; there
//! is no merge, split, recenter or hysteresis. Labels are actor-local scene
//! identities only: never persisted, never reused within the actor and never
//! recycled into item ids. `Ball::pos` is the only box input; millisecond
//! interpolation samples never move a ball between boxes.

use super::*;

/// carbonengine destiny Partition coarsest cell edge in metres.
pub(in crate::space) const BUBBLE_EDGE: f64 = 7_864_320.0;

/// One grid box's integer coordinates. Each axis is `floor(pos / edge)`, so
/// negative coordinates floor into distinct cells on each side of zero.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BoxKey { x: i64, y: i64, z: i64 }

/// A monotonic bubble label. It names one scene partition inside this actor
/// only; it is not durable and is not derived from any entity identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BubbleId(i64);

/// One occupied box: the label it adopted at creation and its ball ids.
struct Cell { label: BubbleId, members: HashSet<i64> }

/// Grid state: occupied boxes, each label's boxes and every ball's cell.
#[derive(Default)]
pub(in crate::space) struct Interest {
    cells: HashMap<BoxKey, Cell>,
    labels: HashMap<BubbleId, HashSet<BoxKey>>,
    placed: HashMap<i64, (BoxKey, BubbleId)>,
    next_label: i64,
}

impl Interest {
    /// The box a position occupies; `None` only for non-finite coordinates.
    fn key(pos: V3) -> Option<BoxKey> {
        if !pos.iter().all(|v| v.is_finite()) { return None; }
        let axis = |v: f64| (v / BUBBLE_EDGE).floor() as i64;
        Some(BoxKey { x: axis(pos[0]), y: axis(pos[1]), z: axis(pos[2]) })
    }

    /// A label that has never been used in this actor.
    fn fresh_label(&mut self) -> BubbleId {
        self.next_label = self.next_label.checked_add(1).expect("bubble label space exhausted");
        BubbleId(self.next_label)
    }

    /// The label a new occupant of `key` joins: the box's own label, the first
    /// labelled box among its 26 neighbours in fixed offset order, or none.
    fn label_for(&self, key: BoxKey) -> Option<BubbleId> {
        if let Some(cell) = self.cells.get(&key) { return Some(cell.label); }
        for dx in -1..=1_i64 {
            for dy in -1..=1_i64 {
                for dz in -1..=1_i64 {
                    if dx == 0 && dy == 0 && dz == 0 { continue; }
                    if let Some(cell) = self.cells.get(&BoxKey { x: key.x + dx, y: key.y + dy, z: key.z + dz }) {
                        return Some(cell.label);
                    }
                }
            }
        }
        None
    }

    /// Place or move one ball by its current `Ball::pos`. Its previous
    /// occupancy releases first; a non-finite position keeps no cell.
    pub(in crate::space) fn place(&mut self, id: i64, pos: V3) {
        let key = Self::key(pos);
        if let Some(&(old, _)) = self.placed.get(&id) {
            if Some(old) == key { return; }
        }
        self.remove(id);
        let Some(key) = key else { return };
        let label = self.label_for(key).unwrap_or_else(|| self.fresh_label());
        self.cells.entry(key).or_insert_with(|| Cell { label, members: HashSet::new() }).members.insert(id);
        self.labels.entry(label).or_default().insert(key);
        self.placed.insert(id, (key, label));
    }

    /// Release one ball's box. An emptied box is dropped, and a label with no
    /// remaining boxes is freed; boxes and labels never merge or move.
    pub(in crate::space) fn remove(&mut self, id: i64) {
        let Some((key, label)) = self.placed.remove(&id) else { return };
        let Some(cell) = self.cells.get_mut(&key) else { return };
        cell.members.remove(&id);
        if !cell.members.is_empty() { return; }
        self.cells.remove(&key);
        if let Some(boxes) = self.labels.get_mut(&label) {
            boxes.remove(&key);
            if boxes.is_empty() { self.labels.remove(&label); }
        }
    }

    /// The label of the box currently holding `id`, if it is placed.
    pub(in crate::space) fn bubble_of(&self, id: i64) -> Option<BubbleId> {
        self.placed.get(&id).map(|(_, label)| *label)
    }
}

/// Celestial groups authorized for the always-present roster layer: suns,
/// planets, moons, asteroid belt waypoints and stargates. NPC stations join
/// by their group and loaded always-global sovereignty hubs by theirs;
/// ores, NPCs and other structures are never promoted.
const GLOBAL_GROUPS: [i32; 6] = [
    infinity_core::consts::GROUP_SUN, infinity_core::consts::GROUP_PLANET,
    infinity_core::consts::GROUP_MOON, infinity_core::consts::GROUP_ASTEROID_BELT,
    infinity_core::consts::GROUP_STARGATE, infinity_core::consts::GROUP_SOVEREIGNTY_HUB,
];

impl System {
    /// The bubble label of the box currently holding `ball`, if placed.
    pub(crate) fn bubble_of(&self, ball: i64) -> Option<BubbleId> {
        self.interest.bubble_of(ball)
    }
    /// Both balls are placed and occupy the same labelled scene partition.
    pub(crate) fn same_bubble(&self, a: i64, b: i64) -> bool {
        self.bubble_of(a).is_some() && self.bubble_of(a) == self.bubble_of(b)
    }
    /// The scene visibility rule on borrowed balls for callers that
    /// temporarily hold the source outside `balls` (command application,
    /// tractor bookkeeping): the source must be the same ball the map
    /// still holds or has already released, the target the map's own live
    /// ball — never a second authority and never a whole-ball clone.
    pub(in crate::space) fn local_pair_from(&self, source: &Ball, target: &Ball) -> bool {
        if self.ball_token(source.id).is_none() { return false; }
        if self.ball(source.id).is_some_and(|ball| !std::ptr::eq(ball, source)) { return false; }
        if source.id == target.id { return std::ptr::eq(source, target); }
        self.ball(target.id).is_some_and(|ball| std::ptr::eq(ball, target))
            && target.cloak == 0
            && (self.membership.globals.contains(&target.id) || self.same_bubble(source.id, target.id))
    }
    /// One token the ball carries now, without allocating one.
    pub(in crate::space) fn ball_token(&self, id: i64) -> Option<observers::BallToken> {
        self.ball_tokens.get(&id).map(|&serial| observers::BallToken::new(id, serial))
    }
    /// Whether a ball joins the always-present roster layer: its loaded flag
    /// plus an authorized group or an always-global hub. Without loaded
    /// static data the admission flag stands alone.
    pub(super) fn roster_global(&self, ball: &Ball) -> bool {
        if !ball.global { return false; }
        let Some(group) = self.sde().and_then(|sde| sde.types.get(ball.type_id)).and_then(|ty| ty.group_id) else { return true };
        GLOBAL_GROUPS.contains(&group) || i64::from(group) == infinity_core::consts::GROUP_STATION
    }
}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
