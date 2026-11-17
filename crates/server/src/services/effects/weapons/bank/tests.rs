//! Real grouped-turret bank regressions against `start_with_roll`: the same
//! core `bank::start` enters with the production `turret_roll`, over a real
//! fixture: fitted guns, `game.module_charges` consumption, the resident
//! combat fit, native generation/sequence admission, real actor damage and
//! reports. The injected closure only counts rolls and records chances; DEBUG
//! lines naming each mismatching field are source-review evidence, not asserted.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::combat::tests::remote_repair_db::set_locked_target;
use crate::services::combat::{self, ActiveModule, ATTR_DAMAGE, capacity::is_refusal, state};
use crate::services::effects::lifecycle::{ActivationOwner, Module, NativeCombatGeneration};
use crate::services::effects::{EffectKind, registry, tests::fixture as fx};
use crate::services::effects::weapons::damage_message;
use crate::services::drones::tests::fixture::Fixture;
use crate::services::fit::{Fit, Key};
use crate::dao::items::{charges, dogma};
use crate::space::targeting::next_generation;
use crate::space::snapshot::facts_test_init;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_MODULE, FILETIME_SECOND, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST,
    dogma::{ATTR_DAMAGE_MULTIPLIER, ATTR_TRACKING_SPEED, EFFECT_GUNNERY_MAX_RANGE_FALLOFF_TRACKING_SPEED_BONUS}};
use marshal::Value;
use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};

mod spool;

/// One armed bank: six charge-fed turrets, a mid-slot tracking computer, a
/// piloted victim and the shared activation state a real bank cycle needs.
struct Rig {
    f: Fixture,
    cx: Ctx,
    guns: Vec<i64>,
    gun: i32,
    ammo2: i32,
    victim: i64,
    victim_char: i64,
    stats: ShipStats,
    effect: registry::Effect,
    stop: Arc<AtomicBool>,
    rx: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>,
}
async fn rig() -> Rig { rig_effect(None).await }

/// `wanted` pins the picked gun to modules carrying that exact firing effect;
/// `None` keeps the original gunnery-skill turret bias.
async fn rig_effect(wanted: Option<i32>) -> Rig {
    let mut f = Fixture::real_system().await;
    let (victim_char, victim) = f.add_pilot().await;
    f.world.with(f.system, move |s| s.command(victim, |ball, _, _| {
        ball.pos = [5_000.0, 0.0, 0.0]; ball.old_pos = ball.pos; vec![]
    })).await.unwrap();
    let fit = Fit::load(&f.db, f.sde.clone(), victim_char, victim).await.unwrap();
    let mut stats = ShipStats::from_fit(&fit, Key::Ship);
    stats.max = [1e6; 3]; // A padded victim keeps every volley in the shield layer.
    let install = stats.clone();
    f.world.with(f.system, move |s| s.install_condition(victim,
        Condition { now: install.max, stats: install, shield_at: crate::client::filetime_now() + 60 * FILETIME_SECOND }, fit)).await.unwrap();
    let sde = f.sde.clone();
    let skill = sde.dogma.effect(EFFECT_GUNNERY_MAX_RANGE_FALLOFF_TRACKING_SPEED_BONUS).unwrap()
        .modifiers().iter().find(|m| m.func.as_str() == "LocationRequiredSkillModifier").and_then(|m| m.skill).unwrap();
    let size = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let fits = |module: i32, charge: i32| {
        let (m, c) = (sde.types.get(module).unwrap(), sde.types.get(charge).unwrap());
        ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
            .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| m.attribute(a.attribute_id))
            .any(|group| c.group_id.is_some_and(|g| group == f64::from(g)))
            && size.and_then(|id| m.attribute(id)).is_none_or(|s| s <= 0.0 || size.and_then(|id| c.attribute(id)) == Some(s))
    };
    let (gun, ammo, ammo2, effect) = sde.types.iter()
        .filter(|m| m.published == Some(true) && m.category_id == Some(CATEGORY_MODULE)
            && combat::targeting_charges::requires_loaded_charge(&sde, m.type_id)
            && match wanted { Some(wanted) => m.effects().iter().any(|e| e.effect_id == wanted),
                None => sde.skill_plans.requirements(m.type_id).iter().any(|&(s, _)| s == skill) })
        .find_map(|m| {
            let effect = m.effects().iter().filter(|e| wanted.is_none_or(|wanted| e.effect_id == wanted))
                .find_map(|e| registry::resolve(&sde, e.effect_id)
                    .filter(|effect| effect.kind == EffectKind::Turret))?;
            let mut charges = sde.types.iter().filter(|c| c.published == Some(true)
                && c.category_id == Some(CATEGORY_CHARGE)
                && ATTR_DAMAGE.iter().any(|&a| c.attribute(a).is_some_and(|v| v > 0.0)) && fits(m.type_id, c.type_id));
            Some((m.type_id, charges.next()?.type_id, charges.next()?.type_id, effect))
        }).expect("chargeable gunnery turret with two damaging charges");
    let buff = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_GUNNERY_MAX_RANGE_FALLOFF_TRACKING_SPEED_BONUS)).unwrap().type_id;
    let mut guns = Vec::new();
    for i in 0..6 {
        let flag = FLAG_HI_SLOT_FIRST + i;
        let gun_id: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id",
            &[&gun, &f.owner, &f.ship, &flag]).await.unwrap().get("item_id");
        f.db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,$4)",
            &[&f.ship, &flag, &ammo, &3_i64]).await.unwrap();
        // A distinct effective multiplier per member makes every raw provably its own.
        dogma::set_override(&f.db, gun_id, ATTR_DAMAGE_MULTIPLIER, 2.0 + i as f64).await.unwrap();
        guns.push(gun_id);
    }
    f.db.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true)",
        &[&buff, &f.owner, &f.ship, &FLAG_MED_SLOT_FIRST]).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(f.owner, tx.clone());
    f.world.register_pilot(victim_char, tx.clone());
    set_locked_target(f.owner, victim, true);
    let stop = Arc::new(AtomicBool::new(false));
    state().active.insert(guns[0], ActiveModule { char_id: f.owner, stop: stop.clone(), forced_inactive: false });
    state().activation_generation.insert(guns[0], stop.clone());
    let cx = Ctx { db: f.db.clone(), world: f.world.clone(), notify: tx, char_id: f.owner, ship_id: f.ship, system_id: f.system };
    Rig { f, cx, guns, gun, ammo2, victim, victim_char, stats, effect, stop, rx }
}

impl Rig {
    /// Reseed one gun's actor loaded slot from durable truth: a committed
    /// absolute incarnation replaces whatever a capture already observed.
    async fn reseed_member(&self, member: usize, row: Option<charges::Loaded>) {
        let (ship, module, flag) = (self.f.ship, self.guns[member], FLAG_HI_SLOT_FIRST + member as i32);
        let seed = row.map(|row| crate::space::combat::loaded::LoadedSeed {
            module, flag, type_id: Some(row.type_id), quantity: row.quantity, pending: false });
        self.f.world.with(self.f.system, move |s| {
            assert!(s.reseed_loaded(ship, vec![(charges::LoadedSlotKey { ship, module, flag }, seed)]));
        }).await.unwrap();
    }
    async fn set_charge(&self, member: usize, charge: i32) {
        self.f.db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,$4)
            ON CONFLICT (ship_id,flag_id) DO UPDATE SET type_id=EXCLUDED.type_id,quantity=EXCLUDED.quantity",
            &[&self.f.ship, &(FLAG_HI_SLOT_FIRST + member as i32), &charge, &3_i64]).await.unwrap();
        self.reseed_member(member, Some(charges::Loaded { type_id: charge, quantity: 3 })).await;
    }
    async fn drop_charge(&self, member: usize) {
        self.f.db.execute("DELETE FROM game.module_charges WHERE ship_id=$1 AND flag_id=$2",
            &[&self.f.ship, &(FLAG_HI_SLOT_FIRST + member as i32)]).await.unwrap();
        self.reseed_member(member, None).await;
    }
    async fn arm(&self, members: usize) -> Cycle {
        fx::publish_fit(&self.f.db, &self.f.world, self.f.system, self.f.owner, self.f.ship).await;
        for member in 0..self.guns.len() {
            let row = charges::loaded(&self.f.db, self.f.ship, FLAG_HI_SLOT_FIRST + member as i32).await.unwrap();
            self.reseed_member(member, row).await;
        }
        self.cycle(members).await
    }
    async fn cycle(&self, members: usize) -> Cycle {
        let guns = &self.guns[..members];
        fx::capture_cycle(Cycle { cx: self.cx.clone(), effect: self.effect.clone(), target: Some(self.victim),
            modules: guns.iter().enumerate().map(|(i, &id)| Module { id, type_id: self.gun, flag: FLAG_HI_SLOT_FIRST + i as i32 }).collect(),
            repeat: 1, stop: self.stop.clone(),
            owner: ActivationOwner { master: guns[0], generation: self.stop.clone(), pending_combat: None,
                world: Arc::downgrade(&self.cx.world), system: self.cx.system_id,
                // `rig` plants and resets the shared activation rows; an unarmed
                // owner still posts real generation retirement on drop.
                armed: false,
                combat_generations: guns.iter().map(|&id| NativeCombatGeneration { module_id: id, generation: next_generation(), sequence: 0 }).collect() },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1_000.0, nominal_start: 0, other: None,
            repair_amount: 0.0, illegal: false, source_membership: None, target_type: None, inputs: Default::default() }).await
    }
    async fn volley(&self, cycle: &mut Cycle, multiplier: f64) -> (usize, Vec<f64>, Result<CycleOutcome, CallError>) {
        let (rolls, chances) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(Vec::new())));
        let (counter, log) = (rolls.clone(), chances.clone());
        let outcome = start_with_roll(&self.cx, cycle, self.victim, move |chance| {
            counter.fetch_add(1, Ordering::Relaxed); log.lock().unwrap().push(chance); multiplier }).await;
        let recorded = chances.lock().unwrap().clone();
        (rolls.load(Ordering::Relaxed), recorded, outcome)
    }
    async fn fire(&self, cycle: &mut Cycle, members: &[usize], multiplier: f64, apps: u64)
        -> (usize, Vec<f64>, Result<CycleOutcome, CallError>, f64)
    {
        let (rev0, before) = self.snap().await;
        let expected = self.expected(cycle, members, multiplier);
        let (rolls, chances, outcome) = self.volley(cycle, multiplier).await;
        let (rev1, after) = self.snap().await;
        assert_eq!(rev1 - rev0, apps, "accepted applications differ");
        let delta: f64 = before.now.iter().zip(after.now).map(|(old, new)| old - new).sum();
        let target = if apps == 0 { 0.0 } else { expected };
        assert!((delta - target).abs() <= 1e-6, "condition delta {delta} vs expected {target}");
        (rolls, chances, outcome, expected)
    }
    fn hits(&mut self) -> Vec<Value> {
        std::iter::from_fn(|| self.rx.try_recv().ok())
            .filter(|(event, _)| event.as_str() == "OnDamageMessage").map(|(_, args)| args[0].clone()).collect()
    }
    fn field<'a>(hit: &'a Value, name: &'static str) -> Option<&'a Value> {
        match hit { Value::Dict(entries) => entries.iter().find(|(k, _)| k == &Value::str(name)).map(|(_, v)| v), _ => None }
    }
    fn banked(hit: &Value) -> bool { Self::field(hit, "isBanked") == Some(&Value::Bool(true)) }
    fn unbanked(hit: &Value) -> bool { Self::field(hit, "isBanked") == Some(&Value::Bool(false)) }
    async fn snap(&self) -> (u64, Condition) {
        let victim = self.victim;
        self.cx.world.with(self.cx.system_id, move |s| {
            let snap = s.combat_condition_snapshot(victim).unwrap();
            (snap.revision, snap.condition)
        }).await.unwrap()
    }
    /// Durable oracle: the snapshot batch is the only writer, so flush first.
    async fn ammo(&self, member: usize) -> i64 {
        self.f.world.flush_positions().await.unwrap();
        charges::loaded(&self.f.db, self.f.ship, FLAG_HI_SLOT_FIRST + member as i32).await.unwrap().map_or(0, |l| l.quantity)
    }
    /// Expected dealt for `members`, mirroring the engine's order: summed raw, scaled once, resonated per type.
    fn expected(&self, cycle: &Cycle, members: &[usize], multiplier: f64) -> f64 {
        let mut raw = [0.0; 4];
        for &i in members {
            match &cycle.inputs.members[i].packet {
                MemberPacket::Turret(inputs) => for (sum, value) in raw.iter_mut().zip(inputs.raw) { *sum += value; },
                _ => panic!("member packet is not turret"),
            }
        }
        raw.iter().zip(self.stats.resonance[0].iter()).map(|(r, s)| r * multiplier * s).sum()
    }
    async fn finish(self) { self.f.cleanup().await; }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn banked_volley_rolls_once_sums_member_raw_and_reports() {
    let mut r = rig().await;
    let mut cycle = r.arm(6).await;
    let (rolls, _, outcome, expected) = r.fire(&mut cycle, &[0, 1, 2, 3, 4, 5], 1.0, 1).await;
    assert_eq!((rolls, outcome.unwrap()), (1, CycleOutcome::Continue));
    assert_eq!(r.hits(), vec![
        damage_message("target", r.victim, r.gun, expected, hit_quality(1.0), true),
        damage_message("source", r.f.ship, r.gun, expected, hit_quality(1.0), true)]);
    for i in 0..6 { assert_eq!(r.ammo(i).await, 2, "each member consumed exactly one charge"); }
    // The actor's effective view agrees with the flushed durable rows.
    let (ship, system, guns) = (r.f.ship, r.f.system, r.guns.clone());
    let view = r.f.world.with(system, move |s| guns.iter().map(|&m| s.loaded_effective(ship, m).map(|v| v.2)).collect::<Vec<_>>()).await.unwrap();
    assert_eq!(view, vec![Some(2); 6]);
    // A superseded activation cancels before any consumption or report and
    // leaves the replacement generation untouched.
    let mut cycle = r.cycle(6).await;
    let replacement = Arc::new(AtomicBool::new(false));
    {
        let mut state = state();
        state.active.insert(r.guns[0], ActiveModule { char_id: r.f.owner, stop: replacement.clone(), forced_inactive: false });
        state.activation_generation.insert(r.guns[0], replacement.clone());
    }
    let (rolls, _, outcome, _) = r.fire(&mut cycle, &[], 1.0, 0).await;
    assert_eq!((rolls, outcome.unwrap()), (0, CycleOutcome::Stop("activation lost")));
    assert!(!replacement.load(Ordering::Relaxed), "a stale volley must not stop the new generation");
    assert!(r.hits().is_empty() && r.ammo(0).await == 2);
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn partial_round_keeps_master_attribution_and_member_sequences() {
    let mut r = rig().await;
    let mut cycle = r.arm(6).await;
    // Master empties before fire, member 3's replay sequence is exhausted, and
    // member 4's loaded charge drifts after capture: members 1, 2 and 5 remain.
    r.drop_charge(0).await;
    cycle.owner.combat_generations[3].sequence = u64::MAX;
    r.set_charge(4, r.ammo2).await;
    let (rolls, _, outcome, expected) = r.fire(&mut cycle, &[1, 2, 5], 1.0, 1).await;
    assert_eq!((rolls, outcome.unwrap()), (1, CycleOutcome::Continue));
    assert_eq!(r.hits(), vec![
        damage_message("target", r.victim, r.gun, expected, hit_quality(1.0), true),
        damage_message("source", r.f.ship, r.gun, expected, hit_quality(1.0), true)]);
    // Sealed journal evidence: the volley is attributed to the master while a
    // slave owned the replay identity. The capture must precede the first
    // `ammo` read: its flush persists and acknowledges every pending batch.
    let (system, master) = (r.cx.system_id, r.guns[0]);
    let events = r.cx.world.with(system, move |s| crate::space::snapshot::capture_fact_checkpoint(s).unwrap()
        .map(|frame| frame.events().map(|e| e.event).collect::<Vec<_>>())).await.unwrap().unwrap_or_default();
    assert_eq!(cycle.owner.combat_generations.iter().map(|g| g.sequence).collect::<Vec<_>>(),
        [0, 1, 1, u64::MAX, 0, 1], "only admitted members advanced a real sequence");
    assert_eq!(r.ammo(0).await, 0, "the empty master consumed nothing");
    // Member 4's drifted charge is never debited: the captured permit refuses
    // the reseeded incarnation instead of paying the wrong type.
    for i in 1..6 { assert_eq!(r.ammo(i).await, if i == 4 { 3 } else { 2 }); }
    assert!(events.iter().any(|event| matches!(event, crate::lua::events::GameEvent::DamageApplied { module_id, .. }
        if *module_id == Some(master))), "banked volley facts attribute the master module");
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn mismatched_inputs_select_per_member_fallback() {
    let mut r = rig().await;
    r.set_charge(4, r.ammo2).await;
    dogma::set_override(&r.f.db, r.guns[3], ATTR_TRACKING_SPEED, 9.0e9).await.unwrap();
    let mut cycle = r.arm(6).await;
    cycle.owner.combat_generations[3].sequence = u64::MAX;
    // Member 3 differs from the master's roll inputs and still fails prepare; member 4's charge differs.
    let fired = [0, 1, 2, 4, 5];
    let (rolls, _, outcome, _) = r.fire(&mut cycle, &fired, 1.0, 5).await;
    assert_eq!((rolls, outcome.unwrap()), (5, CycleOutcome::Continue), "one roll per fired fallback member");
    let hits = r.hits();
    assert_eq!(hits.len(), fired.len() * 2);
    let expected: Vec<f64> = fired.iter().map(|&i| r.expected(&cycle, &[i], 1.0)).collect();
    let dealt: Vec<f64> = hits.iter().filter_map(|hit| Rig::field(hit, "damage").and_then(|v| v.as_float())).collect();
    for value in &expected { assert_eq!(dealt.iter().filter(|&&d| d == *value).count(), 2, "each member's own raw, never averaged"); }
    assert!(hits.iter().all(Rig::unbanked));
    assert_eq!(cycle.owner.combat_generations.iter().map(|g| g.sequence).collect::<Vec<_>>(), [1, 1, 1, u64::MAX, 1, 1]);
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn misses_empty_bank_and_fully_resisted_hit() {
    let mut r = rig().await;
    // All members miss: one roll, one banked miss per observer, once ammo.
    let mut cycle = r.arm(6).await;
    let (rolls, _, outcome, _) = r.fire(&mut cycle, &[0, 1, 2, 3, 4, 5], 0.0, 0).await;
    assert_eq!((rolls, outcome.unwrap()), (1, CycleOutcome::Continue));
    assert_eq!(r.hits(), vec![
        damage_message("target", r.victim, r.gun, 0.0, hit_quality(0.0), true),
        damage_message("source", r.f.ship, r.gun, 0.0, hit_quality(0.0), true)]);
    for i in 0..6 { assert_eq!(r.ammo(i).await, 2); }
    // A fully resisted hit keeps the real non-miss quality with zero dealt.
    let mut stats = r.stats.clone();
    stats.resonance = [[0.0; 4]; 3];
    let (victim, system) = (r.victim, r.f.system);
    let fit = Fit::load(&r.f.db, r.f.sde.clone(), r.victim_char, r.victim).await.unwrap();
    r.cx.world.with(system, move |s| s.install_condition(victim,
        Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() + 60 * FILETIME_SECOND }, fit)).await.unwrap();
    let mut cycle = r.cycle(6).await;
    let (rolls, _, outcome, _) = r.fire(&mut cycle, &[0, 1, 2, 3, 4, 5], 1.0, 0).await;
    assert_eq!((rolls, outcome.unwrap()), (1, CycleOutcome::Continue));
    let hits = r.hits();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| Rig::field(hit, "hitQuality").and_then(|v| v.as_int()) == Some(hit_quality(1.0))
        && Rig::field(hit, "damage").and_then(|v| v.as_float()) == Some(0.0) && Rig::banked(hit)));
    // Zero candidates: no consumption, roll or report.
    for i in 0..6 { r.drop_charge(i).await; }
    let mut cycle = r.cycle(6).await;
    let (rolls, _, outcome) = r.volley(&mut cycle, 1.0).await;
    assert_eq!((rolls, outcome.unwrap()), (0, CycleOutcome::Stop("out of charges")));
    assert!(r.hits().is_empty());
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn prepare_full_cancels_entire_bank_round() {
    let mut r = rig().await;
    let mut cycle = r.arm(6).await;
    // Real bounded capacity: three replay registrations admit members 0-2;
    // member 3's refusal cancels the whole round: canonical error, nothing applied.
    let system = r.cx.system_id;
    r.cx.world.with(system, move |s| facts_test_init(s, 3, 4)).await.unwrap();
    let (rolls, _, outcome, _) = r.fire(&mut cycle, &[], 1.0, 0).await;
    assert!(is_refusal(&outcome.expect_err("capacity refusal propagates")), "canonical capacity error");
    assert_eq!((rolls, r.hits().is_empty()), (0, true));
    assert!(cycle.inputs.members.iter().all(|member| member.turret_round.is_none()), "every armed token closed");
    assert_eq!(cycle.owner.combat_generations.iter().map(|g| g.sequence).collect::<Vec<_>>(), [1, 1, 1, 1, 0, 0],
        "admission consumed the refused member's sequence and stopped without inventing tokens");
    // Closure is real: the freed slot takes the same generation's next
    // sequence again, while the spent sequence stays replay-fenced.
    let (ship, master, generation, weapon) = (r.f.ship, r.guns[0], cycle.owner.combat_generations[0].generation, r.gun);
    let (reused, fenced) = r.cx.world.with(system, move |s| {
        let next = s.prepare_combat_source(ship, Some(master), None, Some(weapon),
            crate::lua::events::resources::FactOrigin::Natural, generation, 2, 1, |_| true);
        if let Ok(facts) = &next { s.finish_combat_attempt(facts); }
        (next.is_ok(), s.prepare_combat_source(ship, Some(master), None, Some(weapon),
            crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).is_err())
    }).await.unwrap();
    assert!(reused, "a closed slot is reusable at the replay layer");
    assert!(fenced, "the spent sequence can never replay");
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn application_full_cancels_banked_volley_without_damage() {
    let mut r = rig().await;
    let mut cycle = r.arm(6).await;
    // Real bounded capacity at the application: admission succeeds, the volley's reservation refuses.
    let system = r.cx.system_id;
    r.cx.world.with(system, move |s| facts_test_init(s, 6, 0)).await.unwrap();
    let (rolls, _, outcome, _) = r.fire(&mut cycle, &[], 1.0, 0).await;
    assert!(is_refusal(&outcome.expect_err("application capacity refusal propagates")));
    assert_eq!((rolls, r.hits().is_empty()), (1, true));
    assert!(cycle.inputs.members.iter().all(|member| member.turret_round.is_none()));
    let before = r.snap().await;
    // A stale frozen target generation refuses in-actor without a roll or shot.
    for index in 0..cycle.modules.len() {
        let member_id = cycle.modules[index].id;
        let attempt = prepare_turret_member(&mut cycle, member_id, true).await.unwrap().expect("member admitted");
        let member = &mut cycle.inputs.members[index];
        member.turret_round = Some(TurretRound { charge: member.charge_type, attempt: Some(attempt), shot: None, spool_bonus: None });
    }
    let context = BankContext { ship: r.f.ship, pilot: r.f.owner, system, master: r.guns[0], weapon: r.gun,
        illegal: false, source_membership: cycle.source_membership, target_generation: next_generation(), stop: r.stop.clone() };
    let rolls = Arc::new(AtomicUsize::new(0));
    let counter = rolls.clone();
    let resolution = bank_resolve(&r.cx, &mut cycle, r.victim, context, true, move |_| { counter.fetch_add(1, Ordering::Relaxed); 1.0 }).await.unwrap();
    assert!(matches!(resolution, BankResolution::TargetLost));
    assert_eq!((rolls.load(Ordering::Relaxed), r.snap().await.1.now), (0, before.1.now));
    assert!(r.hits().is_empty());
    assert!(cycle.inputs.members.iter().all(|member| member.turret_round.is_none()));
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn fallback_application_full_keeps_committed_member_damage() {
    let mut r = rig().await;
    dogma::set_override(&r.f.db, r.guns[2], ATTR_TRACKING_SPEED, 9.0e9).await.unwrap();
    let mut cycle = r.arm(3).await;
    // Real bounded capacity: member 0's three-layer reservation commits, member
    // 1's is refused, member 2 never rolls; committed damage and reports stand.
    let system = r.cx.system_id;
    r.cx.world.with(system, move |s| facts_test_init(s, 3, 4)).await.unwrap();
    let (rolls, _, outcome, _) = r.fire(&mut cycle, &[0], 1.0, 1).await;
    assert!(is_refusal(&outcome.expect_err("fallback application refusal propagates")));
    assert_eq!(rolls, 2, "the refused member rolled once; its successor never rolled");
    let hits = r.hits();
    assert_eq!(hits.len(), 2, "earlier members keep their unbanked reports");
    assert!(hits.iter().all(Rig::unbanked));
    assert!(cycle.inputs.members.iter().all(|member| member.turret_round.is_none()));
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn fallback_first_member_kill_stops_later_rolls() {
    let mut r = rig().await;
    dogma::set_override(&r.f.db, r.guns[3], ATTR_TRACKING_SPEED, 9.0e9).await.unwrap();
    let mut cycle = r.arm(6).await;
    // One hit point per layer: the first fallback member's accepted damage
    // kills, and every later member closes without a roll.
    let (victim, victim_char, system) = (r.victim, r.victim_char, r.cx.system_id);
    let stats = r.snap().await.1.stats;
    let fit = Fit::load(&r.f.db, r.f.sde.clone(), victim_char, victim).await.unwrap();
    r.cx.world.with(system, move |s| s.install_condition(victim,
        Condition { now: [1.0; 3], stats, shield_at: s.combat_observed_time() }, fit)).await.unwrap();
    let (rolls, _, outcome) = r.volley(&mut cycle, 1.0).await;
    assert_eq!(rolls, 1, "the first member's kill ends the round before any later roll");
    assert!(matches!(outcome.unwrap(), CycleOutcome::Ended { reason: "target lost", .. }));
    assert!(r.cx.world.ball(system, victim).await.is_none(), "settlement removed the killed victim");
    let hits = r.hits();
    assert_eq!(hits.len(), 2, "only the killing member reports, no ghost miss for later members");
    assert!(hits.iter().all(Rig::unbanked));
    assert!(cycle.inputs.members.iter().all(|member| member.turret_round.is_none()));
    r.finish().await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn active_tracking_computer_refreshes_member_packet() {
    let mut r = rig().await;
    let mut cycle = r.arm(6).await;
    let optimal = match &cycle.inputs.members[0].packet {
        MemberPacket::Turret(inputs) => inputs.parameters.0, _ => panic!("member packet is not turret") };
    // Past optimal so the computer's bonus measurably raises the real chance.
    let (victim, ship, system) = (r.victim, r.f.ship, r.cx.system_id);
    let (sent, received) = tokio::sync::oneshot::channel();
    r.cx.world.with(system, move |s| s.command(victim, move |ball, _, _| {
        ball.pos = [optimal * 2.0, 0.0, 0.0]; ball.old_pos = ball.pos; let _ = sent.send(()); vec![] })).await.unwrap();
    received.await.expect("victim reposition action");
    assert_eq!(r.cx.world.ball(system, victim).await.unwrap().pos, [optimal * 2.0, 0.0, 0.0]);
    let (rolls, before_chance, outcome, _) = r.fire(&mut cycle, &[0, 1, 2, 3, 4, 5], 1.0, 1).await;
    assert_eq!((rolls, outcome.unwrap()), (1, CycleOutcome::Continue));
    let tc = r.f.db.query_one("SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3",
        &[&r.f.owner, &r.f.ship, &FLAG_MED_SLOT_FIRST]).await.unwrap().get::<_, i64>("item_id");
    assert!(r.cx.world.with(system, move |s| s.fitted_module_active(ship, tc, true)).await.unwrap());
    let mut cycle = r.cycle(6).await;
    let (rolls, on_chance, outcome, _) = r.fire(&mut cycle, &[0, 1, 2, 3, 4, 5], 1.0, 1).await;
    assert_eq!((rolls, outcome.unwrap()), (1, CycleOutcome::Continue));
    assert!(on_chance[0] > before_chance[0], "the active computer raises the real roll input");
    assert!(r.cx.world.with(system, move |s| s.fitted_module_active(ship, tc, false)).await.unwrap());
    let mut cycle = r.cycle(6).await;
    let (rolls, off_chance, outcome, _) = r.fire(&mut cycle, &[0, 1, 2, 3, 4, 5], 1.0, 1).await;
    assert_eq!((rolls, outcome.unwrap()), (1, CycleOutcome::Continue));
    assert_eq!(off_chance, before_chance, "deactivation restores the resident roll input");
    let hits = r.hits();
    assert_eq!(hits.len(), 6, "one banked report per observer per volley");
    assert!(hits.iter().all(Rig::banked));
    r.finish().await;
}

/// A launcher debits exactly one charge at fire; the impact spends nothing.
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn missile_launch_spends_one_charge_and_impact_spends_none() {
    let mut f = Fixture::real_system().await;
    let (_, victim) = f.add_pilot().await;
    let sde = f.sde.clone();
    let (weapon, ammo) = sde.types.iter().filter(|m| m.published == Some(true) && m.category_id == Some(CATEGORY_MODULE)
        && m.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_USE_MISSILES)).find_map(|m| {
        let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
            .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| m.attribute(a.attribute_id)).collect();
        sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
            && c.group_id.is_some_and(|g| groups.contains(&f64::from(g)))
            && c.attribute(infinity_core::consts::dogma::ATTR_EXPLOSION_DELAY).is_some_and(|v| v > 0.0 && v < 60_000.0))
            .map(|c| (m.type_id, c.type_id))
    }).expect("missile launcher and compatible charge");
    let module_id: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id",
        &[&weapon, &f.owner, &f.ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
    f.db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,2)",
        &[&f.ship, &FLAG_HI_SLOT_FIRST, &ammo]).await.unwrap();
    fx::publish_fit(&f.db, &f.world, f.system, f.owner, f.ship).await;
    let (ship, system) = (f.ship, f.system);
    f.world.with(system, move |s| {
        assert!(s.reseed_loaded(ship, vec![(charges::LoadedSlotKey { ship, module: module_id, flag: FLAG_HI_SLOT_FIRST },
            Some(crate::space::combat::loaded::LoadedSeed { module: module_id, flag: FLAG_HI_SLOT_FIRST, type_id: Some(ammo), quantity: 2, pending: false }))]));
    }).await.unwrap();
    set_locked_target(f.owner, victim, true);
    let stop = Arc::new(AtomicBool::new(false));
    { let mut state = state();
        state.active.insert(module_id, ActiveModule { char_id: f.owner, stop: stop.clone(), forced_inactive: false });
        state.activation_generation.insert(module_id, stop.clone()); }
    let cx = Ctx { db: f.db.clone(), world: f.world.clone(), notify: f.client.notify_tx.clone(), char_id: f.owner, ship_id: f.ship, system_id: f.system };
    let effect = registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_USE_MISSILES).expect("missile effect");
    let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
        target: Some(victim), repeat: 1, stop: stop.clone(), owner: ActivationOwner { master: module_id, generation: stop, armed: true,
            combat_generations: vec![NativeCombatGeneration { module_id, generation: next_generation(), sequence: 0 }],
            pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
            source_membership: None, target_type: None, inputs: Default::default() };
    cycle = fx::capture_cycle(cycle).await;
    assert_eq!(crate::services::effects::behaviors::on_cycle_start(&cx, &mut cycle, false).await.unwrap(), CycleOutcome::Continue);
    assert_eq!(f.world.with(system, move |s| s.loaded_effective(ship, module_id)).await.flatten().map(|v| (v.1, v.2)), Some((ammo, 1)), "launch debits exactly once");
    assert!(f.world.after(system, 70_000).await, "impact window elapsed");
    f.world.flush_positions().await.unwrap();
    assert_eq!(charges::loaded(&f.db, ship, FLAG_HI_SLOT_FIRST).await.unwrap().map(|l| (l.type_id, l.quantity)), Some((ammo, 1)),
        "the impact debits nothing more");
    f.cleanup().await;
}
