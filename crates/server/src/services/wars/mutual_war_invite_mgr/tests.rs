//! Database-backed mutual-war authorization, lifecycle and deadline regressions.
use super::*;
use crate::{client::Client, dao::wars::{self, State}, db::Db, scheduler::Job, services::{Call, CallError, CallResult}, space::World};
use std::sync::Arc;

async fn rpc(c: &mut Client, actor: i64, method: &str, args: &[Value]) -> CallResult {
    c.session.apply(vec![("charid", Value::Int(actor)), ("corpid", Value::Int(1))]);
    let kwargs = Value::Dict(vec![]);
    dispatch(c, &Call { service: "mutualWarInviteMgr", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await.unwrap()
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) -> Vec<(String, Vec<Value>)> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() { events.push(event); }
    events
}

fn no_events(receivers: &mut [tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>]) {
    for rx in receivers { assert!(drain(rx).is_empty()); }
}

fn block_events(receivers: &mut [tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>]) {
    no_events(&mut receivers[..3]);
    for rx in &mut receivers[3..] {
        assert_eq!(drain(rx), vec![("OnMutualWarsUpdated_Remote".to_string(), vec![])]);
    }
}

async fn job(db: &Db, id: &str) -> Job {
    let row = db.query_one("SELECT job_id,kind,due_at,payload FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    Job { id: row.get("job_id"), kind: row.get("kind"), due_at: row.get("due_at"), payload: row.get("payload") }
}

async fn snapshot(db: &Db, a: i64, b: i64) -> (Vec<(i64,i64,i64,i64)>, Vec<(i64,String)>, Vec<(String,i64,String)>) {
    let invites = db.query("SELECT from_owner,to_owner,sent_at,expires_at FROM game.war_invites WHERE from_owner=ANY($1) AND to_owner=ANY($1) ORDER BY from_owner,to_owner", &[&vec![a,b]]).await.unwrap().iter().map(|r| (r.get("from_owner"),r.get("to_owner"),r.get("sent_at"),r.get("expires_at"))).collect();
    let wars = db.query("SELECT war_id,state FROM game.wars WHERE declared_by=ANY($1) AND against=ANY($1) ORDER BY war_id", &[&vec![a,b]]).await.unwrap().iter().map(|r| (r.get("war_id"),r.get("state"))).collect();
    let ids = vec![format!("wars.deadline:invite:{a}:{b}"), format!("wars.deadline:invite:{b}:{a}")];
    let jobs = db.query("SELECT job_id,due_at,payload FROM game.jobs WHERE job_id=ANY($1) OR job_id IN (SELECT 'wars.deadline:start:' || war_id FROM game.wars WHERE declared_by=ANY($2) AND against=ANY($2)) OR job_id IN (SELECT 'wars.deadline:finish:' || war_id FROM game.wars WHERE declared_by=ANY($2) AND against=ANY($2)) ORDER BY job_id", &[&ids,&vec![a,b]]).await.unwrap().iter().map(|r| (r.get("job_id"),r.get("due_at"),r.get("payload"))).collect();
    (invites,wars,jobs)
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn mutual_authority_transactions_and_durable_deadlines() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut accounts = Vec::new(); let mut chars = Vec::new();
    for _ in 0..5 {
        let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('war-test-' || nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
        let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'war-test-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
        accounts.push(account); chars.push(character);
    }
    let mut corps = Vec::new();
    for ceo in [chars[0],chars[3]] {
        let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('war-test-' || nextval('game.corporations_corporation_id_seq')::text,'W' || currval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&ceo]).await.unwrap().get("corporation_id");
        corps.push(corp);
    }
    let (a,b) = (corps[0],corps[1]);
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=ANY($2)", &[&a,&chars[0..3].to_vec()]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=ANY($2)", &[&b,&chars[3..5].to_vec()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$1 WHERE character_id=$2", &[&infinity_core::consts::ROLE_DIRECTOR,&chars[1]]).await.unwrap();
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('war-test-' || nextval('game.alliances_alliance_id_seq')::text,'W' || currval('game.alliances_alliance_id_seq')::text,$1,$1,$2,1) RETURNING alliance_id", &[&a,&chars[0]]).await.unwrap().get("alliance_id");
    let test_db = db.clone(); let actors = chars.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(crate::sde::Sde::load(&test_db).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let mut receivers = Vec::new();
        for actor in &actors {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            world.register_pilot(*actor, tx);
            receivers.push(rx);
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream,address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let hub = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), world.clone());
        let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut c = Client::new(crate::conn::Conn::new(stream,address), cfg, test_db.clone(), sde, hub, world.clone(), login);
        let (ceo,director,member,other,outsider) = (actors[0],actors[1],actors[2],actors[3],actors[4]);
        assert!(super::super::authority::require_owner(&test_db,ceo,a).await.is_ok());
        assert!(super::super::authority::require_owner(&test_db,director,a).await.is_ok());
        assert!(super::super::authority::require_owner(&test_db,member,a).await.is_err());
        assert!(super::super::authority::require_owner(&test_db,other,a).await.is_err());
        for args in [vec![],vec![Value::None],vec![Value::Int(0)]] {
            assert!(matches!(rpc(&mut c,ceo,"SendInviteByPlayer",&args).await, Err(CallError::User { .. })));
        }
        let send = |owner| vec![Value::Int(owner)];
        let response = |owner,accept| vec![Value::Int(owner),Value::Bool(accept)];
        let initial = snapshot(&test_db,a,b).await;
        assert!(matches!(rpc(&mut c,member,"SendInviteByPlayer",&send(b)).await,Err(CallError::User { .. })));
        assert_eq!(snapshot(&test_db,a,b).await,initial);
        no_events(&mut receivers);
        rpc(&mut c,director,"SendInviteByPlayer",&send(b)).await.unwrap();
        for rx in &mut receivers {
            assert_eq!(drain(rx), vec![("OnMutualWarsUpdated_Remote".to_string(), vec![])]);
        }
        assert_ne!(rpc(&mut c,other,"GetPendingInvitesForSession",&[]).await.unwrap(),Value::list([]));
        rpc(&mut c,ceo,"WithdrawInviteByPlayer",&send(b)).await.unwrap();
        assert!(wars::invite(&test_db,a,b,crate::client::filetime_now()).await.unwrap().is_none());
        rpc(&mut c,ceo,"SendInviteByPlayer",&send(b)).await.unwrap();
        rpc(&mut c,other,"RespondToInviteByPlayer",&response(a,false)).await.unwrap();
        assert!(wars::invite(&test_db,a,b,crate::client::filetime_now()).await.unwrap().is_none());
        rpc(&mut c,ceo,"SendInviteByPlayer",&send(b)).await.unwrap();
        let past_expiry = crate::client::filetime_now()-1;
        test_db.execute("UPDATE game.war_invites SET sent_at=$3,expires_at=$4 WHERE from_owner=$1 AND to_owner=$2", &[&a,&b,&(past_expiry-1),&past_expiry]).await.unwrap();
        let expired = wars::Invite { from_owner: a, to_owner: b, sent_at: past_expiry-1, expires_at: past_expiry };
        super::super::deadlines::schedule_invite(&test_db,&expired).await.unwrap();
        let invite_id = format!("wars.deadline:invite:{a}:{b}");
        let stale = job(&test_db,&invite_id).await;
        for rx in &mut receivers { drain(rx); }
        let before = snapshot(&test_db,a,b).await;
        assert!(rpc(&mut c,other,"RespondToInviteByPlayer",&response(a,true)).await.is_err());
        assert_eq!(snapshot(&test_db,a,b).await,before);
        no_events(&mut receivers);
        rpc(&mut c,ceo,"SendInviteByPlayer",&send(b)).await.unwrap();
        let replacement = snapshot(&test_db,a,b).await;
        super::super::deadlines::handle(test_db.clone(),world.clone(),stale).await.unwrap();
        assert_eq!(snapshot(&test_db,a,b).await,replacement);
        rpc(&mut c,ceo,"WithdrawInviteByPlayer",&send(b)).await.unwrap();
        rpc(&mut c,ceo,"SendInviteByPlayer",&send(b)).await.unwrap();
        test_db.execute("UPDATE game.war_invites SET sent_at=$3,expires_at=$4 WHERE from_owner=$1 AND to_owner=$2", &[&a,&b,&(past_expiry-1),&past_expiry]).await.unwrap();
        super::super::deadlines::schedule_invite(&test_db,&expired).await.unwrap();
        for _ in 0..2 { super::super::deadlines::handle(test_db.clone(),world.clone(),job(&test_db,&invite_id).await).await.unwrap(); }
        assert!(wars::invite(&test_db,a,b,0).await.unwrap().is_none());
        for rx in &mut receivers { drain(rx); }
        rpc(&mut c,other,"SetInvitesBlockedByPlayer",&[Value::Bool(true)]).await.unwrap();
        block_events(&mut receivers);
        rpc(&mut c,other,"SetInvitesBlockedByPlayer",&[Value::Bool(true)]).await.unwrap();
        no_events(&mut receivers);
        let blocked = snapshot(&test_db,a,b).await;
        assert!(rpc(&mut c,ceo,"SendInviteByPlayer",&send(b)).await.is_err());
        assert_eq!(snapshot(&test_db,a,b).await,blocked);
        no_events(&mut receivers);
        rpc(&mut c,other,"SetInvitesBlockedByPlayer",&[Value::Bool(false)]).await.unwrap();
        block_events(&mut receivers);
        rpc(&mut c,ceo,"SendInviteByPlayer",&send(b)).await.unwrap();
        for rx in &mut receivers { drain(rx); }
        let before = snapshot(&test_db,a,b).await;
        assert!(matches!(rpc(&mut c,other,"RespondToInviteByPlayer",&[Value::Int(a),Value::Int(2)]).await,Err(CallError::User { .. })));
        assert_eq!(snapshot(&test_db,a,b).await,before);
        assert!(matches!(rpc(&mut c,outsider,"RespondToInviteByPlayer",&response(a,true)).await,Err(CallError::User { .. })));
        assert_eq!(snapshot(&test_db,a,b).await,before);
        no_events(&mut receivers);
        rpc(&mut c,other,"SetInvitesBlockedByPlayer",&[Value::Bool(true)]).await.unwrap();
        block_events(&mut receivers);
        let before = snapshot(&test_db,a,b).await;
        assert!(rpc(&mut c,other,"RespondToInviteByPlayer",&response(a,true)).await.is_err());
        assert_eq!(snapshot(&test_db,a,b).await,before);
        no_events(&mut receivers);
        rpc(&mut c,other,"SetInvitesBlockedByPlayer",&[Value::Bool(false)]).await.unwrap();
        block_events(&mut receivers);
        let before = snapshot(&test_db,a,b).await;
        let (mut blocker, connection) = crate::config::tests::test_database().to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let blocker_pid: i32 = blocker.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        let tx = blocker.transaction().await.unwrap();
        tx.query_one("SELECT sent_at FROM game.war_invites WHERE from_owner=$1 AND to_owner=$2 FOR UPDATE", &[&a,&b]).await.unwrap();
        let new_sent = before.0[0].2 + 1;
        let args = response(a,true);
        let (rejected, ()) = tokio::join!(rpc(&mut c,other,"RespondToInviteByPlayer",&args), async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let blocked: bool = test_db.query_one("SELECT EXISTS (SELECT 1 FROM pg_locks l WHERE NOT l.granted AND $1 = ANY(pg_blocking_pids(l.pid))) AS blocked", &[&blocker_pid]).await.unwrap().get("blocked");
                    if blocked { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("acceptance did not reach the locked invite deletion");
            tx.execute("UPDATE game.war_invites SET sent_at=$3 WHERE from_owner=$1 AND to_owner=$2", &[&a,&b,&new_sent]).await.unwrap();
            tx.commit().await.unwrap();
        });
        assert!(matches!(rejected,Err(CallError::User { .. })));
        let mut expected = before;
        expected.0[0].2 = new_sent;
        assert_eq!(snapshot(&test_db,a,b).await,expected);
        no_events(&mut receivers);
        rpc(&mut c,other,"RespondToInviteByPlayer",&response(a,true)).await.unwrap();
        let rows = wars::by_owner(&test_db,a).await.unwrap(); assert_eq!(rows.len(),1);
        let war = &rows[0]; assert_eq!(war.state,State::Pending);
        for rx in &mut receivers {
            let events = drain(rx);
            assert_eq!(events.len(),2);
            assert_eq!(events[0].0,"OnWarChanged");
            assert_eq!(events[0].1,vec![super::super::wire::war(war),Value::list([Value::Int(a),Value::Int(b)]),super::super::wire::changes(None,war)]);
            assert_eq!(events[1],("OnMutualWarsUpdated_Remote".to_string(),vec![]));
        }
        assert!(wars::invite(&test_db,a,b,crate::client::filetime_now()).await.unwrap().is_none());
        let before = snapshot(&test_db,a,b).await;
        assert!(rpc(&mut c,other,"RespondToInviteByPlayer",&response(a,true)).await.is_err());
        assert_eq!(snapshot(&test_db,a,b).await,before);
        no_events(&mut receivers);
        let start_id = format!("wars.deadline:start:{}",war.war_id);
        let future = job(&test_db,&start_id).await;
        super::super::deadlines::handle(test_db.clone(),world.clone(),future).await.unwrap();
        assert_eq!(wars::get(&test_db,war.war_id).await.unwrap().unwrap().state,State::Pending);
        let past = crate::client::filetime_now()-1;
        test_db.execute("UPDATE game.wars SET declared_at=$3,starts_at=$2 WHERE war_id=$1", &[&war.war_id,&past,&(past-1)]).await.unwrap();
        let updated = wars::get(&test_db,war.war_id).await.unwrap().unwrap();
        super::super::deadlines::schedule_start(&test_db,&updated).await.unwrap();
        for _ in 0..2 { super::super::deadlines::handle(test_db.clone(),world.clone(),job(&test_db,&start_id).await).await.unwrap(); }
        assert_eq!(wars::get(&test_db,war.war_id).await.unwrap().unwrap().state,State::Active);
        let system = world.sde().map.systems().find(|s| s.security.is_some_and(|v| v >= crate::config::game().crimewatch.highsec_threshold && v > 0.0)).expect("high-security SDE system").solar_system_id;
        let ship_type = world.sde().types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).expect("SDE ship type").type_id;
        let _attacker_ship = crate::dao::items::transfers::insert(&test_db,ship_type,ceo,system,0,1).await.unwrap();
        crate::dao::items::transfers::insert(&test_db,ship_type,other,system,0,1).await.unwrap();
        let security = crate::services::crimewatch::security_status(&test_db,ceo).await.unwrap();
        let rights: Vec<i64> = crate::dao::kill_rights::mine(&test_db,other,crate::client::filetime_now()).await.unwrap().iter().filter(|r| r.from_id == other && r.to_id == ceo).map(|r| r.kill_right_id).collect();
        for rx in &mut receivers { drain(rx); }
        crate::services::crimewatch::aggress(&test_db,&world,system,ceo,other).await.unwrap();
        assert!(crate::services::crimewatch::weapons_timer(ceo).is_some());
        assert!(crate::services::crimewatch::retaliation_allowed(other,ceo,crate::client::filetime_now()));
        assert!(crate::services::crimewatch::retaliation_allowed(ceo,other,crate::client::filetime_now()));
        assert_eq!(crate::services::crimewatch::security_status(&test_db,ceo).await.unwrap(),security);
        let after_rights: Vec<i64> = crate::dao::kill_rights::mine(&test_db,other,crate::client::filetime_now()).await.unwrap().iter().filter(|r| r.from_id == other && r.to_id == ceo).map(|r| r.kill_right_id).collect();
        assert_eq!(after_rights,rights);
        let attacker_events = drain(&mut receivers[0]);
        assert!(attacker_events.iter().any(|(event,_)| event == "OnWeaponsTimerUpdate"));
        assert!(attacker_events.iter().any(|(event,_)| event == "OnPvpTimerUpdate"));
        assert!(attacker_events.iter().any(|(event,_)| event == "OnCrimewatchEngagementCreated"));
        assert!(!attacker_events.iter().any(|(event,_)| event == "OnCriminalTimerUpdate" || event == "OnSystemCriminalFlagUpdates"));
        let victim_events = drain(&mut receivers[3]);
        assert!(victim_events.iter().any(|(event,_)| event == "OnPvpTimerUpdate"));
        assert!(victim_events.iter().any(|(event,_)| event == "OnCrimewatchEngagementCreated"));
        test_db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&a,&alliance]).await.unwrap();
        test_db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&b,&alliance]).await.unwrap();
        test_db.execute("UPDATE game.characters SET corp_roles=$1 WHERE character_id=$2", &[&infinity_core::consts::ROLE_DIRECTOR,&other]).await.unwrap();
        assert_eq!(super::super::authority::owner(&test_db,ceo).await.unwrap(),alliance);
        assert!(super::super::authority::require_owner(&test_db,director,alliance).await.is_ok());
        assert!(super::super::authority::require_owner(&test_db,member,alliance).await.is_err());
        assert!(super::super::authority::require_owner(&test_db,other,alliance).await.is_err());
        for rx in &mut receivers { drain(rx); }
        let before = snapshot(&test_db,a,b).await;
        assert!(super::super::lifecycle::retract(&test_db,&world,member,war.war_id).await.is_err());
        assert!(super::super::lifecycle::retract(&test_db,&world,outsider,war.war_id).await.is_err());
        assert_eq!(snapshot(&test_db,a,b).await,before);
        no_events(&mut receivers);
        super::super::lifecycle::retract(&test_db,&world,director,war.war_id).await.unwrap();
        for rx in &mut receivers { drain(rx); }
        let before = snapshot(&test_db,a,b).await;
        assert!(super::super::lifecycle::retract(&test_db,&world,ceo,war.war_id).await.is_err());
        assert_eq!(snapshot(&test_db,a,b).await,before);
        no_events(&mut receivers);
        let finish_id = format!("wars.deadline:finish:{}",war.war_id);
        super::super::deadlines::handle(test_db.clone(),world.clone(),job(&test_db,&finish_id).await).await.unwrap();
        assert_eq!(wars::get(&test_db,war.war_id).await.unwrap().unwrap().state,State::Cooldown);
        test_db.execute("UPDATE game.wars SET retracted_at=$3,finishes_at=$2 WHERE war_id=$1", &[&war.war_id,&past,&(past-1)]).await.unwrap();
        let updated = wars::get(&test_db,war.war_id).await.unwrap().unwrap();
        super::super::deadlines::schedule_finish(&test_db,&updated).await.unwrap();
        for _ in 0..2 { super::super::deadlines::handle(test_db.clone(),world.clone(),job(&test_db,&finish_id).await).await.unwrap(); }
        assert_eq!(wars::get(&test_db,war.war_id).await.unwrap().unwrap().state,State::Ended);
        test_db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=ANY($1)", &[&vec![a,b]]).await.unwrap();
        rpc(&mut c,ceo,"SendInviteByPlayer",&send(b)).await.unwrap();
        rpc(&mut c,other,"RespondToInviteByPlayer",&response(a,true)).await.unwrap();
        let next = wars::ongoing_pair(&test_db,a,b,crate::client::filetime_now()).await.unwrap().unwrap();
        test_db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&a,&alliance]).await.unwrap();
        super::super::lifecycle::retract(&test_db,&world,ceo,next.war_id).await.unwrap();
        assert_eq!(wars::get(&test_db,next.war_id).await.unwrap().unwrap().state,State::Cooldown);
    }).await;
    let war_ids: Vec<i64> = db.query("SELECT war_id FROM game.wars WHERE declared_by=ANY($1) AND against=ANY($1)", &[&corps]).await.unwrap().iter().map(|r| r.get("war_id")).collect();
    let mut job_ids = vec![format!("wars.deadline:invite:{a}:{b}"),format!("wars.deadline:invite:{b}:{a}")];
    for id in &war_ids { job_ids.push(format!("wars.deadline:start:{id}")); job_ids.push(format!("wars.deadline:finish:{id}")); }
    db.execute("DELETE FROM game.jobs WHERE job_id=ANY($1)", &[&job_ids]).await.unwrap();
    db.execute("DELETE FROM game.war_invites WHERE from_owner=ANY($1) AND to_owner=ANY($1)", &[&corps]).await.unwrap();
    db.execute("DELETE FROM game.wars WHERE war_id=ANY($1)", &[&war_ids]).await.unwrap();
    db.execute("DELETE FROM game.war_invite_settings WHERE owner_id=ANY($1)", &[&vec![a,b,alliance]]).await.unwrap();
    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=ANY($1)", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id=ANY($1)", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=ANY($1)", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=ANY($1)", &[&accounts]).await.unwrap();
    result.unwrap();
}
