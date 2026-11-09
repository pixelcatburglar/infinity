use super::*;
use crate::dao::sovereignty::{capture::CaptureRow, structures::{HubRow, StructureRow}, systems::SystemRow};
use marshal::Value;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn neutral_hub_uses_actual_type_and_one_capture_slim() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let system = SystemRow { system_id: 7, hub_item_id: Some(11), claim_corporation_id: None,
        alliance_id: None, claim_time: None, ownership_epoch: 0, revision: 1 };
    let hub = HubRow { system: system.clone(), structure: StructureRow { item_id: 11, system_id: 7,
        source_item_id: 12, deployer_character_id: 13, deployer_corporation_id: 14, deployed_at: 1,
        content_namespace: "test".into(), content_version: "test".into(), self_destruct_due_at: None,
        self_destruct_actor: None }, type_id: sde.sovereignty.hub().type_id,
        owner_id: sde.sovereignty.hub().deploy.default_owner, x: Some(1.0), y: Some(2.0), z: Some(3.0) };
    let capture = CaptureRow { target_item_id: 11, system_id: 7, ownership_epoch: 0,
        controlling_team: None, scoring_team: None, defender_team: infinity_core::consts::OWNER_UNOWNED,
        score: 0.0, rate: 0.0, blocked: false, score_sim_time: crate::client::filetime_from_unix_ms(500), checkpoint_wall_time: 0,
        revision: 0, capture_time_seconds: 10.0, defensive_regen_rate: 0.0, checkpoint_interval_ms: 1,
        content_namespace: "test".into(), content_version: "test".into() };
    let ball = super::super::entity::ball(&sde, &hub, &capture, 1000).unwrap();
    assert_eq!(ball.owner_id, hub.owner_id);
    assert_eq!(ball.pos, [1.0, 2.0, 3.0]);
    assert_eq!(ball.global, sde.sovereignty.hub().is_always_global);
    assert!(!ball.slim.iter().any(|(key, _)| *key == "component_active" || *key == "activate"));
    let add = crate::space::add_balls_event(1, &ball, false);
    let call = add.items().unwrap()[1].items().unwrap();
    let args = call[1].items().unwrap();
    let chunk = args[0].items().unwrap();
    assert_eq!(chunk[1].items().unwrap()[0], ball.slim_item());
    let score = &ball.slim.iter().find(|(key, _)| *key == "entosis_score").unwrap().1;
    assert_eq!(*score, Value::tuple([Value::None, Value::None,
        Value::tuple([Value::Float(0.0), Value::Int(capture.score_sim_time), Value::Float(0.0), Value::Bool(false)]),
        Value::Int(infinity_core::consts::OWNER_UNOWNED)]));
    let mut corrupt = hub.clone(); corrupt.x = Some(f64::NAN);
    assert!(super::super::entity::ball(&sde, &corrupt, &capture, 1000).is_err());
    let sde = Arc::new(sde);
    let world = crate::space::World::new(db, sde.clone(), 1);
    assert!(!super::super::is_ready(&world));
    super::super::open(&world);
    assert!(super::super::is_ready(&world));
    super::super::close(&world);
    assert!(!super::super::is_ready(&world));
    let (tx, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(22, tx);
    world.set_online_session(22, Some(infinity_core::session::OnlineSession {
        character_id: 22, user_id: None, session_id: 22, ship_id: None, station_id: None,
        system_id: None, corporation_id: Some(20), alliance_id: Some(30), faction_id: None,
        fleet_id: None, role: 0, corporation_role: 0,
    }));
    let policy = crate::dao::sovereignty::policy::PolicyRow { alliance_id: 30, current_capital_system: Some(7),
        last_capital_changed_at: Some(1), capital_lost_at: None, capital_change_not_before: None, revision: 1 };
    let publication = |changed| CommittedPublication { capitals: vec![crate::services::sovereignty::CapitalPublication {
        policy: policy.clone(), changed,
    }], ..Default::default() };
    super::super::publish(&world, publication(false));
    assert!(notifications.try_recv().is_err());
    super::super::publish(&world, publication(true));
    let (event, args) = notifications.try_recv().unwrap();
    assert_eq!(event, "OnUpdateCapitalInfo");
    assert!(args.is_empty());
    assert!(notifications.try_recv().is_err());
    let fit = crate::services::fit::Fit::for_space_type(sde.clone(), hub.type_id, Vec::new());
    let stats = crate::services::combat::groups_health::ShipStats {
        max: [100.0; 3], resonance: [[1.0; 4]; 3], shield_tau_ms: 1e30,
    };
    let condition = crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now(),
    };
    let mut actor = System::new(7, world.pilots.clone(), 1000);
    actor.projection_sde = Some(sde.clone());
    let original = HubPublication { system, hub: Some(hub.clone()), capture: Some(capture.clone()) };
    super::super::apply(&mut actor, original.clone(), true, &[]).unwrap();
    assert_eq!(actor.balls().filter(|b| b.id == 11).count(), 1);
    assert_eq!(actor.ball(11).unwrap().slim_item(), ball.slim_item());
    let mut pilot = ball.clone();
    pilot.id = 22;
    pilot.char_id = Some(22);
    pilot.free = true;
    actor.insert(pilot);
    assert!(actor.visible_to(22, 11));
    actor.install_condition(22, condition.clone(), crate::services::fit::Fit::for_space_type(sde, hub.type_id, Vec::new()));
    actor.install_condition(11, condition, fit);
    assert_eq!(actor.sovereignty.hub, Some(11));
    let generation = crate::space::targeting::next_generation();
    let make_source = |actor: &mut System, sequence| {
        let facts = actor.prepare_combat_source(22, None, None, None,
            crate::lua::events::resources::FactOrigin::Natural, generation, sequence, 1, |_| true).unwrap();
        crate::services::combat::damage::DamageSource { attacker_id: Some(22), facts: Some(facts), ..Default::default() }
    };
    let source = make_source(&mut actor, 1);
    assert!(actor.apply_damage(11, [10.0, 0.0, 0.0, 0.0], source, false).unwrap().is_none());
    actor.sovereignty.hub = None;
    let source = make_source(&mut actor, 2);
    assert!(actor.apply_damage(11, [10.0, 0.0, 0.0, 0.0], source, false).unwrap().is_some());
    actor.sovereignty.hub = Some(11);
    let mut claimed = original.clone();
    claimed.system.revision += 1;
    claimed.system.ownership_epoch += 1;
    claimed.system.claim_corporation_id = Some(20);
    claimed.system.alliance_id = Some(30);
    claimed.system.claim_time = Some(1);
    let claimed_hub = claimed.hub.as_mut().unwrap();
    claimed_hub.system = claimed.system.clone();
    claimed_hub.owner_id = 20;
    let claimed_target = claimed.capture.as_mut().unwrap();
    claimed_target.ownership_epoch += 1;
    claimed_target.defender_team = 30;
    let expected = crate::services::sovereignty::wire::structures(std::slice::from_ref(claimed.hub.as_ref().unwrap()), std::slice::from_ref(&policy));
    super::super::apply(&mut actor, claimed, false, std::slice::from_ref(&policy)).unwrap();
    let (event, args) = notifications.try_recv().unwrap();
    assert_eq!(event, "OnSolarSystemSovStructuresUpdated");
    assert_eq!(args, vec![Value::Int(7), Value::List(expected)]);
    assert_eq!(notifications.try_recv().unwrap().0, "ProcessSovStatusChanged");
    assert_eq!(notifications.try_recv().unwrap().0, "OnSovereigntyChanged");
    actor.tick(2000);
    assert_eq!((actor.ball(11).unwrap().owner_id, actor.ball(11).unwrap().corp_id, actor.ball(11).unwrap().alliance_id), (20, 20, 30));
    super::super::apply(&mut actor, original, false, &[]).unwrap();
    assert_eq!(actor.ball(11).unwrap().owner_id, 20);
}
