//! Fleet warp acceptance through real fleet RPCs, actor physics and the shared Lua observer.

use std::{future::Future, sync::Arc, time::Duration};
use crate::{client::Client, services::{self, Call}};
use marshal::Value;
use serde_json::json;

async fn rpc(client: &mut Client, service: &str, method: &str, args: &[Value], kwargs: &Value, fleet: Option<i64>) -> services::CallResult {
    let bind = fleet.map(Value::Int);
    services::dispatch(client, &Call { service, method, args, kwargs, bound: bind.is_some(), bind_params: bind.as_ref() }).await
}

pub(super) async fn run(ctx: &super::super::Context) {
    let db = &ctx.resources.db;
    let world = &ctx.resources.world;
    let system = super::SYSTEM;
    let ty = crate::consts::TYPE_CAPSULE;
    let fixtures: Vec<(i64, i64, i64)> = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let mut rows = Vec::new();
        for _ in 0..2 {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('fleet-event-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'fleet-event-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
            let ship: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,$3,0,1,true,0,0,0) RETURNING item_id", &[&ty, &character, &system]).await?.get("item_id");
            rows.push((account, character, ship));
        }
        Ok::<_, crate::db::Error>(rows)
    }), |rows, ()| async move { Ok::<_, crate::db::Error>(rows) }).await.unwrap();
    let target = -fixtures[0].2;
    let mut fleet_id = None;
    let mut clients: Vec<Client> = Vec::new();
    let mut peers = Vec::new();
    let mut work = Box::pin(async {
        let cursor = ctx.events().await.len();
        let cfg = Arc::new(crate::config::Config::default());
        let hub = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        for (index, &(account, character, ship)) in fixtures.iter().enumerate() {
            let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
            let (stream, address) = listener.accept().await.unwrap();
            let login = crate::handshake::Login { user_id: account, user_name: format!("fleet-event-{account}"), role: 0, client_id: ship, session_id: character, language_id: "EN".into() };
            let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg.clone(), db.clone(), ctx.resources.sde.clone(), hub.clone(), world.clone(), login);
            client.change_session(vec![("charid", Value::Int(character)), ("shipid", Value::Int(ship)), ("corpid", Value::Int(1)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
            clients.push(client);
            peers.push(peer);
            // Start separated and aligned to exercise a sustained physical warp.
            let mut ball = super::ball(ship, ty, Some(character), -(index as f64) * 10_000.0);
            ball.vel = [100.0, 0.0, 0.0];
            world.add_ship(system, ball).await.unwrap();
            world.observe(system, ship, ship, clients.last().unwrap().destiny_tx.clone()).await.unwrap();
        }
        world.add_ship(system, super::ball(target, ty, None, 1_000_000_000.0)).await.unwrap();
        rpc(&mut clients[0], "fleetObjectHandler", "CreateFleet", &[], &Value::None, None).await.unwrap();
        let id = clients[0].bound.values().find_map(|(service, value)| (service == "fleetObjectHandler").then(|| value.as_int()).flatten()).unwrap();
        fleet_id = Some(id);
        rpc(&mut clients[0], "fleetObjectHandler", "Init", &[], &Value::None, Some(id)).await.unwrap();
        rpc(&mut clients[0], "fleetObjectHandler", "Invite", &[Value::Int(fixtures[1].1)], &Value::None, Some(id)).await.unwrap();
        rpc(&mut clients[1], "fleetObjectHandler", "AcceptInvite", &[Value::Int(i64::from(ty))], &Value::None, Some(id)).await.unwrap();
        for client in &mut clients {
            let init = rpc(client, "fleetObjectHandler", "GetInitState", &[], &Value::None, Some(id)).await.unwrap();
            assert_eq!(services::value_field(&init, "fleetID"), Some(&Value::Int(id)));
        }
        let args = [Value::str("item"), Value::Int(target)];
        let kwargs = Value::dict([("fleet", Value::Bool(true))]);
        rpc(&mut clients[0], "beyonce", "CmdWarpToStuff", &args, &kwargs, None).await.unwrap();
        let ships = [fixtures[0].2, fixtures[1].2];
        let mut seen = [false; 2];
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let mut stopped = true;
                for (i, ship) in ships.iter().enumerate() {
                    let ball = world.ball(system, *ship).await.expect("fleet ship disappeared");
                    seen[i] |= ball.is_warping();
                    let arrived = seen[i] && ball.mode == crate::space::MODE_STOP;
                    if arrived {
                        assert!(crate::space::dist(ball.pos, [1_000_000_000.0, 0.0, 0.0]) < 10_000.0);
                    }
                    stopped &= arrived;
                }
                if stopped { break; }
                assert!(world.after(system, 100).await);
            }
        }).await.expect("fleet ships did not complete warp");
        let events = ctx.events().await;
        let mut actual = events[cursor..].to_vec();
        actual.sort_by_key(|e| e["char_id"].as_i64().unwrap());
        let mut expected = fixtures.iter().map(|(_, character, _)| json!({"kind":"WarpArrived", "char_id":character, "system_id":system})).collect::<Vec<_>>();
        expected.sort_by_key(|e| e["char_id"].as_i64().unwrap());
        assert_eq!(actual, expected);
    });
    let result = std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.as_mut().poll(cx))) {
        Ok(v) => v.map(Ok), Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await;
    drop(work);
    if let (Some(id), Some(client)) = (fleet_id, clients.first_mut()) {
        if client.session.int("fleetid").is_none() {
            let _ = rpc(client, "fleetObjectHandler", "Init", &[], &Value::None, Some(id)).await;
        }
        let _ = rpc(client, "fleetObjectHandler", "DisbandFleet", &[], &Value::None, Some(id)).await;
    }
    for client in &mut clients {
        client.change_session(vec![("charid", Value::None)]);
        world.unobserve(client.login.client_id);
    }
    for &(_, _, ship) in &fixtures { world.remove_ball(system, ship).await; }
    world.remove_ball(system, target).await;
    let fixtures = [fixtures[0], fixtures[1]];
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        for (account, character, ship) in fixtures {
            tx.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await?;
            tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await?;
            tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await?;
            tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        }
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok::<_, crate::db::Error>(()) }).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
