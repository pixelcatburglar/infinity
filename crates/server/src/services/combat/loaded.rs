//! Thin loaded-charge adapters shared by business roots and the snapshot
//! worker, plus the gate-held loaded sessions every business transaction
//! rides. Callers already hold the running system's checkpoint gate; these
//! functions never take that gate or the market lock themselves and never
//! guess whether an earlier transaction committed.
use crate::dao::items::charges::{self, Charge, LoadedSlotKey, LoadedWrite};
use crate::db::{executor::Executor, Db};
use crate::space::{combat::loaded::LoadedSeed, World};
use crate::space::system::boarding::{HeldLoaded, LoadedQuiesceRefusal};
use super::{db_err, CallError};

/// Absolute-write `rows` inside the caller's transaction. A partially matched
/// batch means actor identity no longer matches durable rows; the attempt
/// fails so the caller treats the outcome as unknown and reseeds under its
/// gate instead of continuing on the stale capture.
pub(crate) async fn settle_in(ex: &(impl Executor + ?Sized), rows: &[LoadedWrite]) -> Result<(), CallError> {
    if rows.is_empty() { return Ok(()); }
    let matched = charges::save_absolute(ex, rows).await.map_err(db_err)?;
    if matched.len() != rows.len() {
        return Err(CallError::Internal(anyhow::anyhow!(
            "loaded settle matched {} of {} slots", matched.len(), rows.len())));
    }
    Ok(())
}

/// One transaction's projection of a touched `(module, flag)` pair's
/// post-business truth: the real module now at the flag, its charge row and
/// pending reservation. A flag whose module vanished emits a removal instead
/// of fabricating an empty slot; a flag whose module changed emits both the
/// old module's removal and the new module's seed.
pub(crate) async fn seed_entries(
    ex: &(impl Executor + ?Sized), ship: i64, touched: &[(i64, i32)],
) -> Result<Vec<(LoadedSlotKey, Option<LoadedSeed>)>, CallError> {
    if touched.is_empty() { return Ok(Vec::new()); }
    let flags: Vec<i32> = touched.iter().map(|&(_, flag)| flag).collect();
    let modules = crate::dao::items::fit::modules(ex, ship).await.map_err(db_err)?;
    let charges = crate::dao::items::charges::all(ex, ship).await.map_err(db_err)?;
    let pendings = crate::dao::items::pending_reloads::for_flags(ex, ship, &flags).await.map_err(db_err)?;
    let mut entries = Vec::new();
    for &(module, flag) in touched {
        let real = modules.iter().find(|m| m.flag_id == flag);
        if real.is_none_or(|m| m.item_id != module) {
            entries.push((LoadedSlotKey { ship, module, flag }, None));
        }
        if let Some(module) = real {
            let charge = charges.iter().find(|c| c.flag_id == flag);
            entries.push((LoadedSlotKey { ship, module: module.item_id, flag }, Some(LoadedSeed {
                module: module.item_id, flag: module.flag_id, type_id: charge.map(|c| c.type_id),
                quantity: charge.map_or(0, |c| c.quantity),
                pending: pendings.iter().any(|row| row.flag_id == flag) })));
        }
    }
    Ok(entries)
}

/// Fresh-read the real loaded rows, pending reservations and module identity
/// covering `slots`, then reseed the actor under the caller's gate. The fresh
/// transaction takes the same per-ship hull lock the loaded writers take, so
/// it waits out any earlier transaction instead of racing it. Success only:
/// a failed read or reseed leaves the slots held and unknown for the next
/// snapshot period. No cargo, refund or other business side effect is
/// replayed here.
pub(crate) async fn reseed_held(db: &Db, world: &World, system: i64, slots: &[LoadedSlotKey]) -> Result<(), CallError> {
    let mut by_ship: std::collections::BTreeMap<i64, Vec<LoadedSlotKey>> = Default::default();
    for key in slots { by_ship.entry(key.ship).or_default().push(*key); }
    let ships = db.transaction(|tx, _: &mut ()| {
        let by_ship = by_ship.clone();
        Box::pin(async move {
            let mut ships = Vec::with_capacity(by_ship.len());
            for (ship, keys) in &by_ship {
                crate::dao::items::ship::lock_reload_ship(tx, *ship).await.map_err(db_err)?;
                let touched: Vec<_> = keys.iter().map(|key| (key.module, key.flag)).collect();
                ships.push((*ship, seed_entries(tx, *ship, &touched).await?));
            }
            Ok::<_, CallError>(ships)
        })
    }, |value, ()| async move { Ok(value) }).await?;
    for (ship, entries) in ships {
        let applied = world.with(system, move |s| s.reseed_loaded(ship, entries)).await
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped during loaded reseed")))?;
        if !applied {
            return Err(CallError::Internal(anyhow::anyhow!("loaded reseed found no combat record for {ship}")));
        }
    }
    Ok(())
}
/// Committed-outcome disposition under the caller's gate: unholds only the
/// slots the receipt still owns after the publication ran.
pub(crate) async fn commit_disposition(world: &World, system: i64, mut hold: HeldLoaded) {
    world.with(system, move |s| hold.release_committed(s)).await;
}

/// Unknown-outcome disposition under the caller's gate: invalidate the
/// receipt's scope, then one fresh reseed; a failed reseed stays held and
/// unknown for the next snapshot period.
pub(crate) async fn unknown_disposition(db: &Db, world: &World, system: i64, mut hold: HeldLoaded) {
    let Some(keys) = world.with(system, move |s| hold.invalidate(s)).await else { return };
    if !keys.is_empty() { let _ = reseed_held(db, world, system, &keys).await; }
}

/// The unknown-ready gate every boundary reconcile passes first: retry one
/// narrow fresh reseed for the still-unknown scope of `ships` under the
/// caller's gate; `false` means the hold must wait for the next period.
pub(crate) async fn fresh_gate(db: &Db, world: &World, system: i64, ships: &[i64]) -> bool {
    let ships: std::collections::HashSet<i64> = ships.iter().copied().collect();
    let Some(unresolved) = world.with(system, move |s| {
        s.unknown_loaded_keys().into_iter().filter(|key| ships.contains(&key.ship)).collect::<Vec<LoadedSlotKey>>()
    }).await else { return false };
    unresolved.is_empty() || reseed_held(db, world, system, &unresolved).await.is_ok()
}

/// One engaged ship whose claim guard already owns its loaded slots.
pub(in crate::services) struct EngagedShip {
    system: i64,
    ship: i64,
    hold: HeldLoaded,
}

struct HeldShip {
    system: i64,
    ship: i64,
    membership: u64,
    writes: Vec<LoadedWrite>,
    view: Vec<Charge>,
    hold: HeldLoaded,
}

/// One business root's gate-held loaded sessions for a set of ships. `arm`
/// refuses new loaded use and waits out admitted finite work before the
/// caller's market lock; `lock` takes each engaged system's snapshot gate in
/// ascending order and captures the held absolutes plus the effective view
/// under it. The caller settles `settle_rows` inside its own transaction,
/// arms `may_execute` right before it, then applies exactly one outcome:
/// `commit` publishes the new absolutes and releases still-owned slots, and
/// `unknown` invalidates the scope and reseeds once under the same gates.
pub(in crate::services) struct LoadedHolds {
    gates: Vec<(i64, tokio::sync::OwnedMutexGuard<()>)>,
    ships: Vec<HeldShip>,
}

impl LoadedHolds {
    /// Quiesce `(system, ship)` pairs: every resident scope is claimed
    /// all-or-nothing, and an actor, quiescing or competing-hold failure is an
    /// explicit refusal rather than a cold pass. `busy` is the calling root's
    /// own normal refusal; only a stopped actor stays internal. Claims are
    /// sorted once; a later refusal drops the earlier guards, which post
    /// their own release.
    pub(in crate::services) async fn arm(world: &World, ships: &[(i64, i64)], busy: CallError) -> Result<Vec<EngagedShip>, CallError> {
        let mut ships = ships.to_vec();
        ships.sort_unstable();
        ships.dedup();
        let mut engaged = Vec::with_capacity(ships.len());
        for (system, ship) in ships {
            let hold = match crate::space::system::boarding::quiesce_ship_loaded(world, system, &[ship]).await {
                Ok(hold) => hold,
                Err(refusal) => return Err(refusal.into_call_error(busy)),
            };
            engaged.push(EngagedShip { system, ship, hold });
        }
        Ok(engaged)
    }

    /// Lock each engaged system's gate in the arm order and capture the held
    /// absolutes and effective view. A lost gate or actor is the stopped
    /// internal error; a claimed scope whose record or membership vanished
    /// is the caller's `busy` refusal. Either way that ship's guard drops and
    /// posts its own rollback release; a ship that claimed no resident scope
    /// keeps its cold answer instead of fabricating an empty hold.
    pub(in crate::services) async fn lock(world: &World, engaged: Vec<EngagedShip>, busy: CallError) -> Result<Self, CallError> {
        let mut holds = Self { gates: Vec::new(), ships: Vec::new() };
        for EngagedShip { system, ship, hold } in engaged {
            if !holds.gates.iter().any(|&(s, _)| s == system) {
                let gate = world.lock_condition_snapshot(system).await.map_err(db_err)?;
                holds.gates.push((system, gate));
            }
            let claimed = hold.claimed_membership();
            let captured = world.with(system, move |s| (
                s.combat_membership_generation(ship),
                s.capture_held_loaded(&[ship]),
                s.loaded_rows(ship),
            )).await;
            match captured {
                Some((membership, writes, view)) => match (claimed, membership, view) {
                    (Some(expected), Some(membership), Some(view)) if expected == membership => {
                        holds.ships.push(HeldShip { system, ship, membership, writes, view, hold });
                    }
                    (None, ..) => continue,
                    _ => return Err(busy),
                },
                None => return Err(LoadedQuiesceRefusal::Stopped.into_call_error(busy)),
            }
        }
        Ok(holds)
    }

    /// Arm every guard's in-flight disposition right before the caller's
    /// transaction may execute.
    pub(in crate::services) fn may_execute(&mut self) {
        for held in &mut self.ships { held.hold.may_execute(); }
    }

    /// The captured held absolutes to settle inside the caller's transaction.
    pub(in crate::services) fn settle_rows(&self) -> Vec<LoadedWrite> {
        self.ships.iter().flat_map(|held| held.writes.iter().cloned()).collect()
    }

    /// Whether `ship` engaged a resident record.
    pub(in crate::services) fn held(&self, ship: i64) -> bool {
        self.ships.iter().any(|held| held.ship == ship)
    }

    /// Every ship that engaged a resident record.
    pub(in crate::services) fn ships(&self) -> Vec<i64> {
        self.ships.iter().map(|held| held.ship).collect()
    }

    /// All effective `flag -> (type, quantity)` rows of `ship` under the hold.
    pub(in crate::services) fn view(&self, ship: i64) -> std::collections::HashMap<i32, (i32, i64)> {
        self.ships.iter().find(|held| held.ship == ship)
            .map(|held| held.view.iter().map(|c| (c.flag_id, (c.type_id, c.quantity))).collect())
            .unwrap_or_default()
    }

    /// Known commit: publish each ship's new absolutes and removals under the
    /// same gate, clear the settled dirt and release the slots the receipt
    /// still owns. A replaced or retired record makes the publication a
    /// no-op; the receipt match leaves nothing to release either.
    pub(in crate::services) async fn commit(mut self, world: &World, seeds: &[(i64, Vec<(LoadedSlotKey, Option<LoadedSeed>)>)]) {
        for held in std::mem::take(&mut self.ships) {
            let entries = seeds.iter().find(|&(ship, _)| *ship == held.ship)
                .map(|(_, entries)| entries.clone()).unwrap_or_default();
            let mut hold = held.hold;
            world.with(held.system, {
                let (ship, membership) = (held.ship, held.membership);
                let saved = held.writes;
                move |s| {
                    let seeds: Vec<LoadedSeed> = entries.iter().filter_map(|(_, seed)| *seed).collect();
                    if s.publish_loaded_absolute(ship, membership, seeds) {
                        let removals: Vec<_> = entries.iter().filter(|(_, seed)| seed.is_none()).copied().collect();
                        if !removals.is_empty() { s.reseed_loaded(ship, removals); }
                        s.clear_saved_loaded(&saved);
                    }
                    hold.release_committed(s);
                }
            }).await;
        }
    }

    /// Unknown outcome — any post-transaction error counts: invalidate each
    /// receipt's scope, then one fresh reseed per ship under the same gates.
    /// A failed reseed leaves the slots held for the next snapshot period.
    pub(in crate::services) async fn unknown(mut self, db: &Db, world: &World) {
        for held in std::mem::take(&mut self.ships) {
            unknown_disposition(db, world, held.system, held.hold).await;
        }
    }

    /// Apply the transaction's outcome in one call: commit publishes `seeds`,
    /// every error — including a business refusal — is unknown.
    pub(in crate::services) async fn finish<R: Send>(self, db: &Db, world: &World, result: Result<R, CallError>,
        seeds: &[(i64, Vec<(LoadedSlotKey, Option<LoadedSeed>)>)]) -> Result<R, CallError> {
        match &result {
            Ok(_) => self.commit(world, seeds).await,
            Err(_) => self.unknown(db, world).await,
        }
        result
    }
}

/// Effective charge rows for `ship`: a resident actor answers from its
/// captured state; a running actor without a record proves nothing is
/// dirty, held or unknown, so the durable rows are complete truth; a
/// stopped actor proves nothing and is refused instead of falling back.
pub(crate) async fn rows(db: &Db, world: &World, system: Option<i64>, ship: i64) -> Result<Vec<charges::Charge>, CallError> {
    if let Some(system) = system {
        match world.with(system, move |s| s.loaded_rows(ship)).await {
            Some(Some(rows)) => return Ok(rows),
            Some(None) => {}
            None => return Err(CallError::Internal(anyhow::anyhow!("system actor stopped during loaded read"))),
        }
    }
    charges::all(db, ship).await.map_err(db_err)
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;
