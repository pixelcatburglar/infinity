//! Guarded capacitor and projected assistance against paid Entosis links.
use crate::services::combat::groups_health::{capacitor_snapshot, drain_exact_guarded, inject_guarded, NpcCapacitorGuard};
use super::super::fixture::Fixture;

pub(super) struct DockBarrier {
    pub world: std::sync::Arc<crate::space::World>,
    pub client: i64,
    pub accepted: Option<tokio::sync::oneshot::Sender<i64>>,
    pub admitted: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl crate::services::ship::observer::DockObserver for DockBarrier {
    fn accepted(&mut self, station: i64) {
        if let Some(sender) = self.accepted.take() { let _ = sender.send(station); }
    }
    async fn wait_pumping(&mut self, system: i64, delay_ms: u64) -> Result<bool, crate::services::CallError> {
        self.admitted.take().unwrap().await.map_err(|_| crate::services::CallError::Internal(anyhow::anyhow!("Entosis admission barrier closed")))?;
        Ok(self.world.after(system, delay_ms).await)
    }
    fn unobserve(&mut self) { self.world.unobserve(self.client); }
}
/// Exercise the native gate transition with a real TCP-backed Client.
pub(super) async fn start_gate(f: &Fixture) -> tokio::task::JoinHandle<(Result<(), crate::services::CallError>, Option<i64>)> {
    use std::sync::Arc;
    use marshal::Value;
    let r = &f.resources;
    let ids = f.ids;
    let destination = r.sde.map.celestial(ids.destination_gate).expect("destination gate celestial");
    let position = [destination.x.unwrap(), destination.y.unwrap(), destination.z.unwrap()];
    let (destiny_tx, mut destiny_rx) = tokio::sync::mpsc::unbounded_channel();
    r.world.observe(ids.systems[0], ids.characters[2], ids.ships[2], destiny_tx).await.expect("observe gate transition");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let config = Arc::new(crate::config::Config::default());
    let chat = crate::chat::Hub::new(&config.chat.domain, r.db.clone(), r.world.clone());
    let login = crate::handshake::Login { user_id: ids.account, user_name: ids.account.to_string(), role: 0, client_id: ids.characters[2], session_id: ids.characters[2], language_id: "EN".into() };
    let mut client = crate::client::Client::new(crate::conn::Conn::new(stream, address), config, r.db.clone(), r.sde.clone(), chat, r.world.clone(), login);
    client.session.apply(vec![("charid", Value::Int(ids.characters[2])), ("corpid", Value::Int(ids.corps[2])), ("shipid", Value::Int(ids.ships[2])), ("solarsystemid", Value::Int(ids.systems[0])), ("solarsystemid2", Value::Int(ids.systems[0]))]);
    let task = tokio::spawn(async move {
        let _peer = peer;
        let jump = crate::services::ship::Jump { through: ids.gate, guid: "effects.JumpOut", other_type: None, dest_system: ids.destination_system, pos: position };
        let result = crate::services::ship::jump_out_and_arrive(&mut client, ids.systems[0], ids.ships[2], ids.characters[2], jump).await;
        (result, client.session.int("solarsystemid"))
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let batch = destiny_rx.recv().await.expect("gate destiny channel");
            if batch.iter().any(|event| {
                let Some(call) = event.items().and_then(|v| v.get(1)).and_then(|v| v.items()) else { return false };
                if call.first() != Some(&Value::str("OnSpecialFX")) { return false; }
                let Some(args) = call.get(1).and_then(|v| v.items()) else { return false };
                args.first().and_then(|v| v.as_int()) == Some(ids.ships[2])
                    && args.get(3).and_then(|v| v.as_int()) == Some(ids.gate)
                    && args.get(5) == Some(&Value::str("effects.JumpOut"))
            }) { break; }
        }
    }).await.expect("native jump-out event deadline");
    task
}

pub(super) async fn guarded_capacitor(f: &Fixture, target: usize, source: usize, blocked: bool) {
    let r = &f.resources;
    let system = f.identity(target).system_id.unwrap();
    let (target_ship, source_ship) = (f.ids.ships[target], f.ids.ships[source]);
    let optimal = r.world.with(system, move |s| {
        let a = s.sampled_ball(source_ship).unwrap();
        let b = s.sampled_ball(target_ship).unwrap();
        crate::space::dist(a.pos, b.pos) + f64::from(a.radius) + f64::from(b.radius) + 1.0
    }).await.unwrap();
    let guard = NpcCapacitorGuard::Attack { source: source_ship, target: target_ship, source_guard: None, player: None, optimal, falloff: 0.0, offensive: false };
    let capacity = capacitor_snapshot(&r.db, &r.world, system, target_ship).await.unwrap().unwrap().capacity;
    assert!(capacity.is_finite() && capacity > 0.0);
    let drained = drain_exact_guarded(&r.db, &r.world, system, target_ship, capacity / 4.0, guard.clone()).await.unwrap().expect("guarded drain must be admitted").0;
    assert!(drained > 0.0);
    let accepted = inject_guarded(&r.db, &r.world, system, target_ship, drained, guard).await.unwrap();
    if blocked { assert_eq!(accepted, 0.0, "remote capacitor assistance must be blocked"); }
    else { assert!(accepted > 0.0, "eligible remote capacitor assistance must be admitted"); }
}

pub(super) async fn self_capacitor(f: &Fixture, ship: usize, other: usize) {
    let r = &f.resources;
    let system = f.ids.systems[0];
    let (ship, other) = (f.ids.ships[ship], f.ids.ships[other]);
    let optimal = r.world.with(system, move |s| {
        let a = s.sampled_ball(ship).unwrap();
        let b = s.sampled_ball(other).unwrap();
        crate::space::dist(a.pos, b.pos) + f64::from(a.radius) + f64::from(b.radius) + 1.0
    }).await.unwrap();
    let guard = NpcCapacitorGuard::Attack { source: ship, target: other, source_guard: None, player: None, optimal, falloff: 0.0, offensive: false };
    let capacity = capacitor_snapshot(&r.db, &r.world, system, ship).await.unwrap().unwrap().capacity;
    let drained = drain_exact_guarded(&r.db, &r.world, system, ship, capacity / 4.0, guard.clone()).await.unwrap().expect("self drain must be admitted").0;
    assert!(drained > 0.0);
    assert!(inject_guarded(&r.db, &r.world, system, ship, drained, guard).await.unwrap() > 0.0, "self capacitor injection must remain admitted");
}

/// Start a real SDE assistance projection before the capital link is admitted.
pub(super) async fn start_projection(f: &Fixture, sender: crate::space::NotifyTx) -> (Vec<i32>, Vec<(f64, f64)>) {
    use crate::services::{combat::{Ctx, targeting_charges}, effects::{lifecycle, registry::{self, EffectFlag}, EffectKind}, fit::{Fit, Key}};
    let r = &f.resources;
    let ids = f.ids;
    let system = ids.systems[0];
    let identity = f.identity(0);
    let type_id: i32 = r.db.query_one("SELECT type_id FROM game.items WHERE item_id=$1", &[&ids.assistance]).await.unwrap().get("type_id");
    let effect_id = r.sde.types.effects(type_id).iter().find_map(|entry| {
        let effect = registry::resolve(&r.sde, entry.effect_id)?;
        (matches!(effect.kind, EffectKind::Projected { .. }) && effect.has(EffectFlag::Assistance)).then_some(entry.effect_id)
    }).expect("fitted projected assistance SDE effect");
    let row = r.sde.dogma.effect(effect_id).unwrap();
    let attrs: Vec<_> = row.modifiers().iter().filter(|row| row.domain == "targetID").map(|row| row.modified).collect();
    assert!(!attrs.is_empty());
    let name = row.name.clone().unwrap();
    let before = Fit::load_in_space(&r.db, &r.world, system, ids.characters[2], ids.ships[2]).await.unwrap();
    let baseline = attrs.iter().map(|&attr| (before.value(Key::Ship, attr), before.value(Key::Module(ids.modules[2]), attr))).collect();
    let cx = Ctx::from_identity(&identity, &r.db, &r.world, Some(system), sender.clone()).unwrap();
    targeting_charges::add_target_ctx(&cx, ids.ships[2], true).await.expect("assistance target request");
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        while !targeting_charges::targets_of(ids.characters[0]).contains(&ids.ships[2]) {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("assistance target lock");
    lifecycle::activate(&identity, &r.db, &r.world, &r.sde, Some(system), sender, ids.account, ids.assistance, &name, Some(ids.ships[2]), 1000, |_| Ok(())).await.expect("projected assistance activation");
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !r.world.with(system, move |s| s.projections_on(ids.ships[2]).iter().any(|p| p.source_module == ids.assistance)).await.unwrap() {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("real assistance projection admission");
    (attrs, baseline)
}

pub(super) async fn projection_values(f: &Fixture, attrs: &[i32]) -> Vec<(f64, f64)> {
    use crate::services::fit::{Fit, Key};
    let ids = f.ids;
    let fit = Fit::load_in_space(&f.resources.db, &f.resources.world, ids.systems[0], ids.characters[2], ids.ships[2]).await.unwrap();
    attrs.iter().map(|&attr| (fit.value(Key::Ship, attr), fit.value(Key::Module(ids.modules[2]), attr))).collect()
}

pub(super) async fn projection_visible(f: &Fixture) -> bool {
    let ids = f.ids;
    f.resources.world.with(ids.systems[0], move |s| s.projections_on(ids.ships[2]).iter().any(|p| p.source_module == ids.assistance)).await.unwrap()
}
