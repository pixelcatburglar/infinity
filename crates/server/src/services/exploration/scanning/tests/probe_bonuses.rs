//! Deployed probes re-read the pilot's resident fit on every RequestScans:
//! an emptied launcher still evaluates its probe type, launched probes keep
//! their launch-time scanBonuses, and burst runtime sources plus module
//! offlining change the next scan of the same deployed probes.

use super::*;
use crate::services::fit::{Fit, Key, SourceScope};
use crate::services::fit::runtime::{RuntimeProducerNamespace, RuntimeReceiverKey, RuntimeSourceKey};
use crate::space::dbuff::TimedRuntimeInput;
use infinity_core::consts::dogma::{ATTR_EXPEDITION_COMMAND_BURST_PROBE_DEVIATION_BONUS, ATTR_EXPEDITION_COMMAND_BURST_PROBE_STRENGTH_BONUS};
use tokio::sync::mpsc::UnboundedReceiver;

const EPS: f64 = 1e-9;

/// One RequestScans moving `probe` over `dest` at `range`.
async fn move_probe(c: &mut Client, probe: i64, dest: V3, range: f64) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let moved = keyval([("destination", pos_value(dest)), ("scanRange", Value::Float(range))]);
    let args = [Value::Dict(vec![(Value::Int(probe), moved)])];
    let call = Call { service: "scanMgr", method: "RequestScans", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dispatch(c, &call).await
}

/// `move_probe` at `AU`, then Started's duration and Stopped's args off the wires.
async fn request_scan(c: &mut Client, rx: &mut UnboundedReceiver<(String, Vec<Value>)>, probe: i64, dest: V3) -> (i64, Vec<Value>) {
    let before = c.outbox.len();
    assert_eq!(move_probe(c, probe, dest, AU).await.unwrap(), Value::None);
    let (_, started) = c.outbox[before..].iter().map(notify).find(|(name, _)| name == "OnSystemScanStarted").unwrap();
    let stopped = loop {
        let (event, args) = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await.unwrap().unwrap();
        if event == "OnSystemScanStopped" { break args; }
    };
    (started[1].as_int().unwrap(), stopped)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn scan_inputs_follow_the_resident_fit() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let system = 910_311_i64;
    let sde = crate::tests::shared_sde(&db).await;
    let fixture = scan_ship(&db, system, false).await;
    let (character, ship, launcher, arrays) = (fixture.character, fixture.ship, fixture.items[0], fixture.items[1..4].to_vec());
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        // The empty launcher's virtual charge and the loaded charge node evaluate
        // the same probe attributes; modules and hull split.
        let empty = Fit::load(&scenario_db, sde.clone(), character, ship).await.unwrap();
        crate::dao::items::combat::apply_loads(&scenario_db, ship, &[(launcher, 27, 30013, 8, 0)]).await.unwrap();
        let loaded = Fit::load(&scenario_db, sde.clone(), character, ship).await.unwrap();
        for (attr, expect) in [(ATTR_BASE_SENSOR_STRENGTH, 55.44), (1372, 0.081)] {
            assert_eq!(empty.charge_value(launcher, 30013, attr, SourceScope::All), loaded.value(Key::Charge(launcher), attr));
            assert_eq!(loaded.charge_value(launcher, 30013, attr, SourceScope::All), loaded.value(Key::Charge(launcher), attr));
            assert!((loaded.value(Key::Charge(launcher), attr) - expect).abs() < EPS);
        }
        assert!((loaded.charge_value(launcher, 30013, ATTR_BASE_SENSOR_STRENGTH, SourceScope::Modules) - 42.0).abs() < EPS);
        assert!((loaded.charge_value(launcher, 30013, ATTR_BASE_SENSOR_STRENGTH, SourceScope::Hull) - 40.0).abs() < EPS);
        client.session.apply(vec![("solarsystemid", Value::Int(system))]);
        client.world.system(system).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(character, tx);
        let mut site = new_site(system, [4e7, 0.0, 0.0], Kind::Signature, STRENGTH_ALL, 1.0, &mut HashSet::new());
        site.id = character * 100 + 81;
        let (site_id, site_pos, target) = (site.id, site.pos, site.target.clone());
        let planted = character * 100 + 71;
        {
            let mut st = state();
            st.loaded.insert(system);
            st.sites.insert(site_id, site);
            st.probes.insert(planted, Probe { char_id: character, system, type_id: 30013, pos: [0.0; 3], expiry: filetime_now() + FILETIME_SECOND * 600, bonuses: ProbeBonuses::default() });
        }
        // Missing shipid and no resident fit are both SPACE_REQUIRED, an
        // offline launcher is ScnNoLauncher; refusals move no probe and send
        // no Started.
        let before = client.outbox.len();
        let space_required = |e: CallError| assert!(matches!(e, CallError::User { msg, dict }
            if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::exploration::SPACE_REQUIRED))]));
        space_required(move_probe(&mut client, planted, [1e9, 0.0, 0.0], AU).await.unwrap_err());
        client.session.apply(vec![("shipid", Value::Int(ship))]);
        space_required(move_probe(&mut client, planted, [1e9, 0.0, 0.0], AU).await.unwrap_err());
        actors::admit(&scenario_db, &client.world, system, actors::ball(ship, 621, character, Some(character), 0.0)).await;
        assert!(matches!(move_probe(&mut client, planted, [1e9, 0.0, 0.0], AU).await.unwrap_err(),
            CallError::User { msg, dict } if msg == messages::MSG_SCN_NO_LAUNCHER && dict.is_empty()));
        assert_eq!(state().probes.get(&planted).unwrap().pos, [0.0; 3]);
        assert!(!client.outbox[before..].iter().map(notify).any(|(name, _)| name == "OnSystemScanStarted"));
        state().probes.remove(&planted);
        assert!(client.world.with(system, move |s| s.fitted_module_online(ship, launcher, true)).await.unwrap());
        // One probe then the remaining seven empty the launcher; every
        // OnNewProbe keeps the launch-time 2x2, and once the charge token is
        // gone the deployed probes still scan.
        let kwargs = Value::Dict(vec![]);
        for count in [1_i64, 7] {
            let args = [Value::Int(launcher), Value::Int(count)];
            let call = Call { service: "dogmaIM", method: "LaunchProbes", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            launch_probes(&mut client, &call).await.unwrap();
        }
        let probes: Vec<i64> = client.outbox.iter().filter_map(|packet| {
            if !matches!(&packet.destination, infinity_wire::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnNewProbe")) { return None; }
            let (_, args) = notify(packet);
            let b = field(&args[0], "scanBonuses").unwrap();
            let p = |key: &str, origin: &str| b.get(key).and_then(|d| d.get(origin)).and_then(Value::as_float).unwrap();
            assert!((p("strength", "modules") - 5.0).abs() < EPS && p("strength", "ship").abs() < EPS
                && (p("deviation", "modules") + 10.0).abs() < EPS && p("deviation", "ship").abs() < EPS);
            field(&args[0], "probeID").and_then(Value::as_int)
        }).collect();
        assert_eq!(probes.len(), 8);
        assert!(client.world.with(system, move |s| s.fitted_charge_token(ship, launcher)).await.unwrap().is_none());
        // The same deployed probe scans the unauthorized signature at 1 AU;
        // duration, certainty and the result sphere all come from the fit.
        let probe = probes[0];
        let floor = crate::config::game().exploration.scan_uncertainty_floor_m;
        let check = |expect: (i64, f64, f64), (ms, stopped): (i64, Vec<Value>)| {
            assert_eq!(ms, expect.0);
            let found = stopped[1].items().unwrap().iter().find(|v| field(*v, "id").and_then(|id| id.as_text()).as_deref() == Some(target.as_str())).unwrap();
            let Value::Instance(_, data) = found else { panic!("scan result is a KeyVal") };
            let (c, radius) = (data.get("certainty").and_then(Value::as_float).unwrap(), data.get("data").and_then(Value::as_float).unwrap());
            assert!((c - expect.1).abs() < EPS, "certainty {c}");
            assert!((radius - (AU * expect.2 * (1.0 - expect.1)).max(floor)).abs() < 1.0, "radius {radius}");
        };
        check((6480, 0.17325, 0.324), request_scan(&mut client, &mut rx, probe, site_pos).await);
        // One burst source lands the strength and deviation collections once:
        // the launcher reads 1.2/0.8, never squared.
        let source = RuntimeSourceKey { producer: RuntimeProducerNamespace::new(9), source_id: 1, incarnation: 1 };
        let receiver = RuntimeReceiverKey::Ship(ship);
        let landed = client.world.with(system, move |s| s.land_runtime_inputs(&[
            TimedRuntimeInput { receiver, collection_id: 2464, source, value: 20.0, expires_at_ms: s.now_ms() + 60_000 },
            TimedRuntimeInput { receiver, collection_id: 2466, source, value: -20.0, expires_at_ms: s.now_ms() + 60_000 },
        ])).await.unwrap();
        assert_eq!(landed, 2);
        let bursts = client.world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| (
            fit.value(Key::Module(launcher), ATTR_EXPEDITION_COMMAND_BURST_PROBE_STRENGTH_BONUS),
            fit.value(Key::Module(launcher), ATTR_EXPEDITION_COMMAND_BURST_PROBE_DEVIATION_BONUS)))).await.unwrap().unwrap();
        assert!((bursts.0 - 1.2).abs() < EPS && (bursts.1 - 0.8).abs() < EPS, "bursts {bursts:?}");
        check((6480, 0.2079, 0.2592), request_scan(&mut client, &mut rx, probe, site_pos).await);
        // Offlining the three arrays drops the same probe to the hull+skill
        // values; the stored launch-time scanBonuses do not move.
        for &array in &arrays {
            assert!(client.world.with(system, move |s| s.fitted_module_online(ship, array, false)).await.unwrap());
        }
        check((7200, 0.198, 0.288), request_scan(&mut client, &mut rx, probe, site_pos).await);
        let b = state().probes.get(&probe).unwrap().bonuses;
        assert!((b.strength_modules - 5.0).abs() < EPS && b.strength_ship == 0.0
            && (b.deviation_modules + 10.0).abs() < EPS && b.deviation_ship == 0.0, "bonuses {b:?}");
        client.world.unregister_pilot_generation(character, None);
        client.world.clone()
    }).await;
    {
        let mut st = state();
        st.probes.retain(|_, p| p.char_id != character || p.system != system);
        st.sites.remove(&(character * 100 + 81));
        st.loaded.remove(&system);
        st.reported.remove(&(character, system));
    }
    match result {
        Ok(world) => fixture::cleanup_ship(&db, Some(&*world), fixture).await.unwrap(),
        Err(error) => { fixture::cleanup_ship(&db, None, fixture).await.unwrap(); std::panic::resume_unwind(error.into_panic()); }
    }
}
