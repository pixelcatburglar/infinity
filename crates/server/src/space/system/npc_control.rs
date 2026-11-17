//! GM NPC-ignore projection inside the system actor: a per-pilot scalar on the
//! pilot index plus a per-drone scalar on DroneBrain, both seeded from the
//! persisted `game.characters.gm_npc_ignored` flag at cold installation and
//! updated for every live actor by a committed command fan-out. NPC target
//! selection, lock completion and queued projections consult only these
//! actor-local scalars; no database, lock or allocation is added to any tick.
//! Ownership transfers clear the drone scalar; the scalar dies with the
//! DroneBrain or pilot entry, leaving no owner tombstones.

use super::*;

impl System {
    /// Persisted flag projected onto `character`'s pilot entry; absent pilots read false.
    pub(in crate::space) fn pilot_npc_ignored(&self, character: i64) -> bool {
        self.pilots.get(&character).is_some_and(|p| p.npc_ignored)
    }
    /// Whether `target` (a ball id) is ineligible as an NPC target: a piloted
    /// ship consults its character's pilot scalar, a drone its owner's scalar
    /// projected on the brain, and anything else is always eligible.
    pub(crate) fn npc_target_ignored(&self, target: i64) -> bool {
        if let Some(character) = self.ball(target).and_then(|b| b.char_id) {
            return self.pilot_npc_ignored(character);
        }
        self.drone_npc_ignored(target)
    }
    /// Seed the prepared flag onto the just-installed pilot entry in the same
    /// actor turn. Returns false unless the entry still maps character to ship.
    pub(crate) fn install_pilot_npc_ignored(&mut self, character: i64, ship: i64, ignored: bool) -> bool {
        let Some(pilot) = self.pilots.get_mut(&character) else { return false };
        if pilot.ship_id != ship { return false; }
        pilot.npc_ignored = ignored;
        true
    }
    /// Apply a committed flag change to the live pilot entry and every drone
    /// brain owned by `character`; `on` also retires all NPC locks on the
    /// character's protected balls through the ordinary loss path.
    pub(crate) fn set_npc_ignored(&mut self, character: i64, ignored: bool) {
        if let Some(pilot) = self.pilots.get_mut(&character) { pilot.npc_ignored = ignored; }
        self.set_owned_drone_npc_ignored(character, ignored);
        if ignored { self.retire_npc_targets_for(character); }
    }
}

impl World {
    /// Enqueue the committed flag into every live system actor's FIFO mailbox
    /// and return one acknowledgement receiver per actor. Each receiver
    /// resolves after that actor applied `set_npc_ignored` (including target
    /// retirement); a failed send drops its sender, so the closed receiver
    /// reports the dead actor instead of counting it as published.
    pub(crate) fn publish_npc_ignored(&self, character: i64, ignored: bool) -> Vec<tokio::sync::oneshot::Receiver<()>> {
        let handles: Vec<_> = crate::poison::lock_or_exit(self.systems.lock()).values().filter(|h| !h.is_closed()).cloned().collect();
        let mut acks = Vec::with_capacity(handles.len());
        for handle in handles {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = handle.send(actor::Job::new(move |s: &mut System| {
                s.set_npc_ignored(character, ignored);
                let _ = tx.send(());
            }));
            acks.push(rx);
        }
        acks
    }
}
