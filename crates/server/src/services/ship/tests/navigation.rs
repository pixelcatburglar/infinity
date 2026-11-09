use super::*;

#[test]
fn goto_point_is_absolute_and_rejects_non_finite() {
    let mut ball = ship();
    let point = [100.0, 200.0, 300.0];
    let events = super::super::navigation::point_command(&mut ball, 77, point);
    assert_eq!(ball.goto, point);
    assert!(ball.speed_fraction > 0.0);
    assert_eq!(events.iter().filter(|e| **e == event(77, "GotoPoint", vec![Value::Int(ball.id), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])).count(), 1);
    let events = super::super::navigation::point_command(&mut ball, 77, [f64::NAN, 0.0, 0.0]);
    assert!(events.is_empty());
    assert_eq!(ball.goto, point);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn unsupported_subject_does_not_warp_to_a_matching_ball() {
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = -1;
    let target_id = -2;
    let world = space::World::new(db.clone(), sde.clone(), 1);
    world.system(system).await.unwrap();
    let mut vessel = ship();
    vessel.id = -3;
    vessel.mode = MODE_STOP;
    vessel.pos = [0.0; 3];
    vessel.old_pos = vessel.pos;
    vessel.vel = [0.0; 3];
    vessel.old_vel = vessel.vel;
    vessel.segment_acceleration = None;
    let mut target = vessel.clone();
    target.id = target_id;
    target.pos = [1000.0, 0.0, 0.0];
    target.old_pos = target.pos;
    let hull = vessel.id;
    world.with(system, move |s| { s.insert(vessel); s.insert(target); }).await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    client.session.apply(vec![("charid", Value::Int(1)), ("shipid", Value::Int(hull)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    let before = world.ball(system, hull).await.unwrap();
    let args = [Value::str("epinstance"), Value::Int(target_id)];
    let call = Call { service: "beyonce", method: "CmdWarpToStuff", args: &args, kwargs: &Value::None, bound: false, bind_params: None };
    assert!(matches!(crate::services::dispatch(&mut client, &call).await, Ok(Value::None)));
    assert!(world.after(system, 1100).await);
    let after = world.ball(system, hull).await.unwrap();
    assert_eq!((after.mode, after.pos, after.goto), (before.mode, before.pos, before.goto));
    world.remove_ball(system, hull).await;
    world.remove_ball(system, target_id).await;
}
