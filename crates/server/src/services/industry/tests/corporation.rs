use super::*;
use crate::consts::{ACCOUNT_CASH, CARGO_CONTAINER_GROUPS, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_HANGAR, INDUSTRY_JOB_STATUS_READY, ROLE_FACTORY_MANAGER, ROLE_FACTORY_MANUFACTURING, ROLE_FACTORY_SCIENCE, ROLE_HANGAR_CAN_TAKE_1};
use crate::db::Db;

mod readiness;

#[derive(Clone, Copy)]
struct Fixture { account: i64, actor: i64, ceo: i64, corp: i64, office: i64, station: i64, system: i64, bp: i32 }

async fn fingerprint(db: &Db, f: Fixture) -> String {
    db.query_one(
        "WITH fixture_trades AS (SELECT * FROM game.trades WHERE char_a=$2 OR char_b=$2)
         SELECT json_build_object(
           'items',(SELECT json_agg(row_to_json(i) ORDER BY item_id) FROM game.items i WHERE owner_id = ANY($1)),
           'blueprints',(SELECT json_agg(row_to_json(b) ORDER BY b.item_id) FROM game.blueprints b JOIN game.items i ON i.item_id=b.item_id WHERE i.owner_id = ANY($1)),
           'jobs',(SELECT json_agg(row_to_json(j) ORDER BY job_id) FROM game.industry_jobs j WHERE installer_id=$2),
           'wallets',(SELECT json_agg(row_to_json(w) ORDER BY account_key) FROM game.corp_wallets w WHERE corporation_id=$3),
           'characters',(SELECT json_agg(row_to_json(c) ORDER BY character_id) FROM game.characters c WHERE user_id=$4),
           'market_orders',(SELECT json_agg(row_to_json(o) ORDER BY order_id) FROM game.market_orders o WHERE char_id=$2 OR type_id=$5),
           'market_transactions',(SELECT json_agg(row_to_json(t) ORDER BY transaction_id) FROM game.market_transactions t WHERE buyer_id=$2 OR seller_id=$2 OR type_id=$5),
           'market_order_history',(SELECT json_agg(row_to_json(h) ORDER BY order_id) FROM game.market_order_history h WHERE char_id=$2 OR type_id=$5),
           'trades',(SELECT json_agg(row_to_json(t) ORDER BY trade_id) FROM fixture_trades t),
           'trade_items',(SELECT json_agg(row_to_json(i) ORDER BY item_id) FROM game.trade_items i WHERE trade_id IN (SELECT trade_id FROM fixture_trades)),
           'trade_timeout_jobs',(SELECT json_agg(row_to_json(j) ORDER BY job_id) FROM game.jobs j WHERE job_id IN (SELECT 'trade-timeout:' || trade_id::text FROM fixture_trades)),
           'industry_deadlines',(SELECT json_agg(row_to_json(j) ORDER BY job_id) FROM game.jobs j WHERE job_id IN (SELECT 'industry.ready:' || job_id::text FROM game.industry_jobs WHERE installer_id=$2))
         )::text AS state",
        &[&vec![f.actor, f.ceo, f.corp], &f.actor, &f.corp, &f.account, &f.bp],
    ).await.unwrap().get("state")
}

fn assert_active<T>(result: Result<T, CallError>) {
    match result {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(crate::messages::industry::BLUEPRINT_ACTIVE))]);
        }
        _ => panic!("expected active blueprint CustomNotify"),
    }
}

async fn item(db: &Db, kind: i32, owner: i64, location: i64, flag: i32, quantity: i32, singleton: bool) -> i64 {
    db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1,$2,$3,$4,$5,$6) RETURNING item_id", &[&kind, &owner, &location, &flag, &quantity, &singleton]).await.unwrap().get("item_id")
}

fn request(f: Fixture, blueprint: i64, activity: i32, input: &authorization::Location, output: &authorization::Location, payer: i64, key: i32) -> Value {
    Value::dict([
        ("blueprintID", Value::Int(blueprint)), ("facilityID", Value::Int(f.station)),
        ("activityID", Value::Int(i64::from(activity))), ("runs", Value::Int(1)), ("licensedRuns", Value::Int(1)),
        ("inputLocation", input.value()), ("outputLocation", output.value()),
        ("account", Value::tuple([Value::Int(payer), Value::Int(i64::from(key))])),
    ])
}

async fn quantities(db: &Db, owner: i64, location: i64, flag: i32, kind: i32) -> i64 {
    db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&owner, &location, &flag, &kind]).await.unwrap().get("n")
}
async fn corporate_runner(db: &Db, client: &Client, f: Fixture, id: i64, deadline: &str, product: i32, output: &authorization::Location) -> (tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
    use std::time::Duration;
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ceo_tx, mut ceo_rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(f.actor, actor_tx);
    client.world.register_pilot(f.ceo, ceo_tx);
    db.execute("DELETE FROM game.jobs WHERE job_id<>$1", &[&deadline]).await.unwrap();
    let due = filetime_now() + crate::client::filetime_ticks_from_seconds(5);
    db.execute("UPDATE game.industry_jobs SET end_date=$2 WHERE job_id=$1", &[&id, &due]).await.unwrap();
    db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&deadline, &due]).await.unwrap();
    let before = quantities(db, f.corp, output.item_id, output.flag_id, product).await;
    let (shutdown, watch) = tokio::sync::watch::channel(false);
    let mut runner = crate::scheduler::runner::start(db.clone(), client.world.clone(), vec![crate::scheduler::register(completion::KIND, completion::handle)], crate::config::scheduler::SchedulerConfig::default(), watch).unwrap();
    crate::scheduler::wake();
    let observed = tokio::spawn(async move {
        assert!(tokio::time::timeout(Duration::from_secs(1), actor_rx.recv()).await.is_err());
        assert!(ceo_rx.try_recv().is_err());
        for receiver in [&mut actor_rx, &mut ceo_rx] {
            let (event, args) = tokio::time::timeout(Duration::from_secs(15), receiver.recv()).await.unwrap().unwrap();
            assert!(filetime_now() >= due);
            assert_eq!(event, "OnIndustryJob");
            assert_eq!(args.get(0).and_then(Value::as_int), Some(id));
            assert_eq!(args.get(1).and_then(Value::as_int), Some(f.corp));
            assert_eq!(args.get(4).and_then(Value::as_int), Some(i64::from(INDUSTRY_JOB_STATUS_READY)));
            assert_eq!(tokio::time::timeout(Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap().0, "OnBlueprintsUpdated");
        }
        (actor_rx, ceo_rx)
    }).await;
    let _ = shutdown.send(true);
    let drained = tokio::time::timeout(Duration::from_secs(10), &mut runner).await;
    if drained.is_err() { runner.abort(); let _ = runner.await; }
    drained.expect("runner did not drain").expect("runner panicked");
    let (mut actor_rx, mut ceo_rx) = observed.expect("corporate readiness notification assertions failed");
    assert!(actor_rx.try_recv().is_err());
    assert!(ceo_rx.try_recv().is_err());
    assert_eq!(quantities(db, f.corp, output.item_id, output.flag_id, product).await, before);
    (actor_rx, ceo_rx)
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn install_delivery_and_refusals_use_real_custody_and_wallets() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let baseline = super::database_fingerprint(&db).await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let facility = sde.map.stations().filter(|s| s.type_id.is_some() && s.solar_system_id.is_some()).filter_map(|s| Facility::from_station(&sde, s)).find(|f| f.factory && f.laboratory).expect("factory and laboratory station");
    let prices: HashMap<i32, f64> = db.query("SELECT type_id, adjusted FROM game.retail_prices", &[]).await.unwrap().into_iter().map(|r| (r.get("type_id"), r.get("adjusted"))).collect();
    let bp = sde.types.iter().find_map(|t| {
        let m = sde.industry.activity(t.type_id, MANUFACTURING)?;
        let c = sde.industry.activity(t.type_id, COPYING)?;
        for activity in [RESEARCH_TIME, RESEARCH_MATERIAL] {
            let research = sde.industry.activity(t.type_id, activity)?;
            if research.time <= 0.0 || research.time >= 1_000_000.0 || research.materials.iter().any(|(_, q)| *q <= 0 || *q >= 1_000_000) { return None; }
        }
        let price = m.materials.iter().map(|(t, q)| prices.get(t).copied().unwrap_or(0.0) * *q as f64).sum::<f64>();
        (sde.is_tradable(t.type_id) && m.time > 0.0 && m.time < 1_000_000.0 && c.time > 0.0 && c.time < 1_000_000.0 && !m.materials.is_empty() && m.products.len() == 1 && m.products[0].1 > 0 && c.max_production_limit >= 1 && m.materials.iter().chain(&c.materials).all(|(_, q)| *q > 0 && *q < 1_000_000) && (10_000.0..100_000_000.0).contains(&price)).then_some(t.type_id)
    }).expect("manufacturing and copying blueprint in SDE");
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('industry-corp-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let actor: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,balance) VALUES ($1,'industry-corp-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,1000000000) RETURNING character_id", &[&account, &facility.id]).await.unwrap().get("character_id");
    let ceo: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'industry-corp-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('industry-corp-' || nextval('game.corporations_corporation_id_seq')::text,'IC' || nextval('game.corporations_corporation_id_seq')::text,$1,$1,$2,0) RETURNING corporation_id", &[&ceo, &facility.id]).await.unwrap().get("corporation_id");
    let roles = ROLE_FACTORY_MANAGER | ROLE_FACTORY_MANUFACTURING | ROLE_FACTORY_SCIENCE | ROLE_HANGAR_CAN_TAKE_1 | (134_217_728_i64 << 1);
    db.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$2 WHERE character_id=$3", &[&corp, &roles, &actor]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=$3 WHERE character_id=$2", &[&corp, &ceo, &facility.id]).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,1000000000)", &[&corp, &(ACCOUNT_CASH + 1)]).await.unwrap();
    let office = item(&db, i32::try_from(facility.type_id).unwrap(), corp, facility.id, FLAG_HANGAR, 1, true).await;
    crate::dao::offices::insert(&db, office, corp, facility.id, 1).await.unwrap();
    let f = Fixture { account, actor, ceo, corp, office, station: facility.id, system: facility.system, bp };
    let saved_jobs: String = db.query_one("SELECT COALESCE(json_agg(row_to_json(j) ORDER BY job_id), '[]'::json)::text AS rows FROM game.jobs j", &[]).await.unwrap().get("rows");
    db.execute("DELETE FROM game.jobs", &[]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        for activity in [MANUFACTURING, COPYING] {
            for &(skill, level) in &sde.industry.activity(f.bp, activity).unwrap().skills {
                db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level)", &[&f.actor, &skill, &level]).await.unwrap();
            }
        }
        let cfg = Arc::new(crate::config::Config::default());
        let world = crate::space::World::new(db.clone(), sde.clone(), 1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: f.account, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db.clone(), sde.clone(), chat, world, login);
        client.session.apply(vec![("charid", Value::Int(f.actor)), ("corpid", Value::Int(f.corp)), ("stationid", Value::Int(f.station)), ("solarsystemid2", Value::Int(f.system))]);
        let personal = authorization::locations(&db, &sde, f.actor, f.actor, &facility).await.ok().unwrap().remove(0);
        let divisions = authorization::locations(&db, &sde, f.actor, f.corp, &facility).await.ok().unwrap();
        let preview_type = sde.industry.activity(f.bp, MANUFACTURING).unwrap().materials[0].0;
        item(&db, preview_type, f.actor, f.station, FLAG_HANGAR, 23, false).await;
        let preview = |location: Value| Value::dict([
            ("blueprintID", Value::None), ("facilityID", Value::Int(f.station)),
            ("inputLocation", location),
        ]);
        let args = [preview(personal.value())];
        let kwargs = Value::None;
        let call = Call { service: "industryMonitor", method: "ConnectJob", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let result = industry_monitor::dispatch(&mut client, &call).await.unwrap().ok().unwrap();
        let values = result.items().unwrap();
        let Value::Dict(materials) = &values[1] else { panic!("monitor materials must be a dictionary") };
        assert!(materials.iter().any(|(kind, quantity)| kind.as_int() == Some(i64::from(preview_type)) && quantity.as_int() == Some(23)));
        for location in [
            Value::instance("industry.job.Location", [("itemID", Value::Int(f.station)), ("flagID", Value::Int(i64::from(FLAG_HANGAR))), ("ownerID", Value::Int(f.ceo))]),
            Value::instance("industry.job.Location", [("itemID", Value::Int(f.office)), ("flagID", Value::Int(i64::from(FLAG_CORP_SAG1))), ("ownerID", Value::Int(f.ceo))]),
        ] {
            let args = [preview(location)];
            let call = Call { args: &args, ..call };
            client.outbox.clear();
            let state = fingerprint(&db, f).await;
            assert!(industry_monitor::dispatch(&mut client, &call).await.unwrap().is_err());
            assert_eq!(fingerprint(&db, f).await, state);
            assert!(client.outbox.is_empty());
        }
        for (owner, input, output, wallet, key, activity) in [
            (f.actor, &personal, &personal, f.actor, ACCOUNT_CASH, MANUFACTURING),
            (f.actor, &personal, &personal, f.corp, ACCOUNT_CASH + 1, MANUFACTURING),
            (f.corp, &divisions[0], &divisions[1], f.actor, ACCOUNT_CASH, MANUFACTURING),
            (f.corp, &divisions[0], &divisions[1], f.corp, ACCOUNT_CASH + 1, MANUFACTURING),
            (f.corp, &divisions[0], &divisions[1], f.corp, ACCOUNT_CASH + 1, COPYING),
        ] {
            let act = sde.industry.activity(f.bp, activity).unwrap();
            let blueprint = item(&db, f.bp, owner, input.item_id, input.flag_id, 1, true).await;
            for &(kind, quantity) in &act.materials {
                let quantity = i32::try_from(quantity.max(1)).unwrap();
                item(&db, kind, owner, input.item_id, input.flag_id, quantity + 10, false).await;
                if owner == f.corp { item(&db, kind, owner, f.office, FLAG_CORP_SAG1 + 2, 7, false).await; }
            }
            let before: Vec<_> = act.materials.iter().map(|&(kind, quantity)| (kind, quantity.max(1))).collect();
            let mut material_before = Vec::new();
            for &(kind, _) in &before { material_before.push(quantities(&db, owner, input.item_id, input.flag_id, kind).await); }
            let mut other_before = Vec::new();
            for &(kind, _) in &before { other_before.push(quantities(&db, owner, f.office, FLAG_CORP_SAG1 + 2, kind).await); }
            let personal_balance: f64 = crate::services::wallets::balance(&db, f.actor).await.ok().unwrap();
            let corp_balance: f64 = crate::services::wallets::corp_balance(&db, f.corp, ACCOUNT_CASH + 1).await.ok().unwrap();
            let req = request(f, blueprint, activity, input, output, wallet, key);
            let id = mechanics::install(&mut client, &req).await.ok().unwrap().as_int().unwrap();
            let job = industry_dao::job(&db, id).await.unwrap().unwrap();
            let deadline = format!("industry.ready:{id}");
            let scheduled = db.query_one("SELECT kind,due_at,payload::text AS payload FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap();
            assert_eq!(scheduled.get::<_, String>("kind"), completion::KIND);
            assert_eq!(scheduled.get::<_, i64>("due_at"), job.end);
            assert_eq!(scheduled.get::<_, String>("payload"), id.to_string());
            let future = filetime_now() + crate::consts::FILETIME_MINUTE;
            db.execute("UPDATE game.industry_jobs SET end_date=$2 WHERE job_id=$1", &[&id, &future]).await.unwrap();
            db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&deadline, &future]).await.unwrap();
            client.outbox.clear();
            let early = fingerprint(&db, f).await;
            assert!(mechanics::complete(&mut client, id).await.is_err());
            assert_eq!(fingerprint(&db, f).await, early);
            assert!(client.outbox.is_empty());
            let policy = &crate::config::game().industry;
            let manufacturing = sde.industry.activity(f.bp, MANUFACTURING).unwrap();
            let value = manufacturing.materials.iter().map(|(kind, quantity)| prices.get(kind).copied().unwrap_or(0.0) * *quantity as f64).sum::<f64>();
            let base_cost = if activity == COPYING { value * policy.job_cost_rate } else { value };
            let rounded = base_cost.round();
            let expected = (base_cost * policy.system_cost_index).round() + (rounded * policy.npc_facility_tax).round()
                + (rounded * policy.scc_surcharge_rate).round()
                + if facility.security >= 0.45 { (rounded * policy.highsec_security_tax).round() } else { 0.0 };
            assert_eq!(job.cost, expected);
            assert!(job.cost > 0.0);
            assert_eq!((job.owner, job.input_location, job.input_flag, job.output_location, job.output_flag, job.wallet_owner, job.wallet_account_key, job.blueprint_location, job.blueprint_flag), (owner, input.item_id, input.flag_id, output.item_id, output.flag_id, wallet, key, input.item_id, input.flag_id));
            for (index, &(kind, amount)) in before.iter().enumerate() {
                assert_eq!(quantities(&db, owner, input.item_id, input.flag_id, kind).await, material_before[index] - amount);
                assert_eq!(quantities(&db, owner, f.office, FLAG_CORP_SAG1 + 2, kind).await, other_before[index]);
            }
            let new_personal = crate::services::wallets::balance(&db, f.actor).await.ok().unwrap();
            let new_corp = crate::services::wallets::corp_balance(&db, f.corp, ACCOUNT_CASH + 1).await.ok().unwrap();
            if wallet == f.actor { assert!((personal_balance - new_personal - job.cost).abs() < 0.001); assert_eq!(corp_balance, new_corp); }
            else { assert_eq!(personal_balance, new_personal); assert!((corp_balance - new_corp - job.cost).abs() < 0.001); }
            if owner == f.actor && wallet == f.actor {
                let container_type = sde.types.iter().find(|t| t.group_id.is_some_and(|g| CARGO_CONTAINER_GROUPS.contains(&g)) && sde.is_tradable(t.type_id)).expect("tradable cargo container in SDE").type_id;
                let container = item(&db, container_type, f.actor, f.station, FLAG_HANGAR, 1, true).await;
                let trade = crate::dao::trades::create(&db, f.station, f.actor, f.ceo, filetime_now()).await.unwrap();
                let bind = Value::Int(trade.id);
                let kwargs = Value::None;
                for status in [INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY] {
                    if status == INDUSTRY_JOB_STATUS_READY {
                        use std::time::Duration;
                        let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
                        let (ceo_tx, mut ceo_rx) = tokio::sync::mpsc::unbounded_channel();
                        client.world.register_pilot(f.actor, actor_tx);
                        client.world.register_pilot(f.ceo, ceo_tx);
                        let due = filetime_now() + crate::consts::FILETIME_MINUTE;
                        db.execute("UPDATE game.industry_jobs SET end_date=$2 WHERE job_id=$1", &[&id, &due]).await.unwrap();
                        db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&deadline, &due]).await.unwrap();
                        db.execute("DELETE FROM game.jobs WHERE job_id<>$1", &[&deadline]).await.unwrap();
                        let (stop, watch) = tokio::sync::watch::channel(false);
                        let mut first_runner = crate::scheduler::runner::start(db.clone(), client.world.clone(), vec![crate::scheduler::register(completion::KIND, completion::handle)], crate::config::scheduler::SchedulerConfig::default(), watch).unwrap();
                        let _ = stop.send(true);
                        let drained = tokio::time::timeout(Duration::from_secs(10), &mut first_runner).await;
                        if drained.is_err() { first_runner.abort(); let _ = first_runner.await; }
                        drained.expect("runner did not drain before restart").expect("runner panicked");
                        let row: String = db.query_one("SELECT row_to_json(j)::text AS row FROM game.jobs j WHERE job_id=$1", &[&deadline]).await.unwrap().get("row");
                        completion::seed(&db).await.ok().unwrap();
                        let after: String = db.query_one("SELECT row_to_json(j)::text AS row FROM game.jobs j WHERE job_id=$1", &[&deadline]).await.unwrap().get("row");
                        assert_eq!(row, after);
                        db.execute("UPDATE game.jobs SET state='running',lease_until=$2,lease_token=123,attempt=2 WHERE job_id=$1", &[&deadline, &due]).await.unwrap();
                        let leased: String = db.query_one("SELECT row_to_json(j)::text AS row FROM game.jobs j WHERE job_id=$1", &[&deadline]).await.unwrap().get("row");
                        completion::seed(&db).await.ok().unwrap();
                        let preserved: String = db.query_one("SELECT row_to_json(j)::text AS row FROM game.jobs j WHERE job_id=$1", &[&deadline]).await.unwrap().get("row");
                        assert_eq!(leased, preserved);
                        db.execute("UPDATE game.jobs SET state='dead',lease_until=NULL,lease_token=NULL WHERE job_id=$1", &[&deadline]).await.unwrap();
                        completion::seed(&db).await.ok().unwrap();
                        assert_eq!(db.query_one("SELECT state FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap().get::<_, String>("state"), "scheduled");
                        db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap();
                        completion::seed(&db).await.ok().unwrap();
                        completion::seed(&db).await.ok().unwrap();
                        assert_eq!(db.query_one("SELECT count(*) AS n FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap().get::<_, i64>("n"), 1);
                        db.execute("DELETE FROM game.jobs WHERE job_id<>$1", &[&deadline]).await.unwrap();
                        let due = filetime_now() + crate::client::filetime_ticks_from_seconds(5);
                        db.execute("UPDATE game.industry_jobs SET end_date=$2 WHERE job_id=$1", &[&id, &due]).await.unwrap();
                        db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&deadline, &due]).await.unwrap();
                        let (shutdown, watch) = tokio::sync::watch::channel(false);
                        let mut runner = crate::scheduler::runner::start(db.clone(), client.world.clone(), vec![crate::scheduler::register(completion::KIND, completion::handle)], crate::config::scheduler::SchedulerConfig::default(), watch).unwrap();
                        crate::scheduler::wake();
                        let observed = tokio::spawn(async move {
                            assert!(tokio::time::timeout(Duration::from_secs(1), actor_rx.recv()).await.is_err());
                            let (event, args) = tokio::time::timeout(Duration::from_secs(15), actor_rx.recv()).await.unwrap().unwrap();
                            assert!(filetime_now() >= due);
                            assert_eq!(event, "OnIndustryJob");
                            assert_eq!(args.get(0).and_then(Value::as_int), Some(id));
                            assert_eq!(args.get(4).and_then(Value::as_int), Some(i64::from(INDUSTRY_JOB_STATUS_READY)));
                            let (event, _) = tokio::time::timeout(Duration::from_secs(5), actor_rx.recv()).await.unwrap().unwrap();
                            assert_eq!(event, "OnBlueprintsUpdated");
                            assert!(tokio::time::timeout(Duration::from_secs(1), actor_rx.recv()).await.is_err());
                            assert!(ceo_rx.try_recv().is_err());
                        }).await;
                        let _ = shutdown.send(true);
                        let drained = tokio::time::timeout(Duration::from_secs(10), &mut runner).await;
                        if drained.is_err() { runner.abort(); let _ = runner.await; }
                        drained.expect("runner did not drain").expect("runner panicked");
                        observed.expect("runner notification assertions failed");
                        assert_eq!(industry_dao::job(&db, id).await.unwrap().unwrap().status, status);
                        let projections = crate::dao::items::fitting::blueprints(&db, owner, Some(blueprint)).await.unwrap();
                        assert_eq!(projections.iter().find(|row| row.item_id == blueprint).unwrap().job_id, Some(id));
                        let pending = industry_dao::jobs_by_owner(&db, owner, false, INDUSTRY_JOB_STATUS_INSTALLED, 0).await.unwrap();
                        assert!(pending.iter().any(|job| job.id == id));
                        let counts = job_counts(&db, f.actor).await.ok().unwrap();
                        assert!(!counts.iter().any(|(activity, count)| *activity == MANUFACTURING && *count != 0));
                        let args = [Value::Int(id), Value::Int(f.system)];
                        let call = Call { service: "industryManager", method: "CancelJob", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                        client.outbox.clear();
                        let state = fingerprint(&db, f).await;
                        assert!(industry_manager::dispatch(&mut client, &call).await.unwrap().is_err());
                        assert_eq!(fingerprint(&db, f).await, state);
                        assert!(client.outbox.is_empty());
                        completion::handle(db.clone(), client.world.clone(), crate::scheduler::Job { id: deadline.clone(), kind: completion::KIND.into(), due_at: 0, payload: id.to_string() }).await.unwrap();
                        assert_eq!(fingerprint(&db, f).await, state);
                    }
                    assert!(!industry_dao::has_active_blueprint_job(&db, &[container, f.office]).await.unwrap());
                    assert!(industry_dao::has_active_blueprint_job(&db, &[container, blueprint, f.office]).await.unwrap());
                    client.outbox.clear();
                    let state = fingerprint(&db, f).await;
                    let destination = crate::services::inventory::access::Container::Item(container);
                    assert_active(crate::services::inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, blueprint, Some(f.station), Some(FLAG_CARGO), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await);
                    assert_eq!(fingerprint(&db, f).await, state);
                    assert!(client.outbox.is_empty());

                    let location = crate::services::inventory::trashing::resolve_location(&client, blueprint).await.unwrap();
                    assert_active(crate::services::inventory::trashing::trash_items(&mut client, &[blueprint], location).await);
                    assert_eq!(fingerprint(&db, f).await, state);
                    assert!(client.outbox.is_empty());

                    let args = [Value::list([keyval([
                        ("stationID", Value::Int(f.station)), ("typeID", Value::Int(i64::from(f.bp))),
                        ("itemID", Value::Int(blueprint)), ("price", Value::Float(1.0)), ("quantity", Value::Int(1)),
                    ])]), Value::Bool(false), Value::Int(0), Value::Float(0.0)];
                    let call = Call { service: "marketProxy", method: "PlaceMultiSellOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                    assert_active(crate::services::market::dispatch(&mut client, &call).await.unwrap());
                    assert_eq!(fingerprint(&db, f).await, state);
                    assert!(client.outbox.is_empty());

                    let args = [Value::Int(blueprint), Value::Int(f.station)];
                    let call = Call { service: "trademgr", method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
                    assert_active(crate::services::trade::route(&mut client, &call).await);
                    assert_eq!(fingerprint(&db, f).await, state);
                    assert!(client.outbox.is_empty());
                    assert!(!crate::services::trade::is_trade_container(trade.id));

                    db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&blueprint, &container, &FLAG_CARGO]).await.unwrap();
                    let nested_state = fingerprint(&db, f).await;
                    let args = [Value::Int(container), Value::Int(f.station)];
                    let call = Call { args: &args, ..call };
                    assert_active(crate::services::trade::route(&mut client, &call).await);
                    assert_eq!(fingerprint(&db, f).await, nested_state);
                    assert!(client.outbox.is_empty());
                    assert!(!crate::services::trade::is_trade_container(trade.id));
                    db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&blueprint, &f.station, &FLAG_HANGAR]).await.unwrap();
                }
                assert_eq!(industry_dao::job(&db, id).await.unwrap().unwrap().status, INDUSTRY_JOB_STATUS_READY);
            }
            let product = if activity == MANUFACTURING { act.products[0].0 } else { f.bp };
            let mut recipients = if owner == f.corp && wallet == f.actor && activity == MANUFACTURING {
                let channels = corporate_runner(&db, &client, f, id, &deadline, product, output).await;
                assert_eq!(industry_dao::job(&db, id).await.unwrap().unwrap().status, INDUSTRY_JOB_STATUS_READY);
                let projections = crate::dao::items::fitting::blueprints(&db, owner, Some(blueprint)).await.unwrap();
                assert_eq!(projections.iter().find(|row| row.item_id == blueprint).unwrap().job_id, Some(id));
                let state = fingerprint(&db, f).await;
                client.outbox.clear();
                let destination = crate::services::inventory::access::Container::Item(f.office);
                assert!(crate::services::inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, blueprint, Some(f.office), Some(output.flag_id), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
                let location = crate::services::inventory::trashing::resolve_location(&client, blueprint).await.unwrap();
                assert!(crate::services::inventory::trashing::trash_items(&mut client, &[blueprint], location).await.is_err());
                let args = [Value::list([keyval([
                    ("stationID", Value::Int(f.station)), ("typeID", Value::Int(i64::from(f.bp))),
                    ("itemID", Value::Int(blueprint)), ("price", Value::Float(1.0)), ("quantity", Value::Int(1)),
                ])]), Value::Bool(false), Value::Int(0), Value::Float(0.0)];
                let kwargs = Value::None;
                let call = Call { service: "marketProxy", method: "PlaceMultiSellOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert!(crate::services::market::dispatch(&mut client, &call).await.unwrap().is_err());
                let trade: i64 = db.query_one("SELECT trade_id FROM game.trades WHERE char_a=$1 OR char_b=$1", &[&f.actor]).await.unwrap().get("trade_id");
                let bind = Value::Int(trade);
                let args = [Value::Int(blueprint), Value::Int(f.station)];
                let call = Call { service: "trademgr", method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
                assert!(crate::services::trade::route(&mut client, &call).await.is_err());
                assert_eq!(fingerprint(&db, f).await, state);
                assert!(client.outbox.is_empty());
                Some(channels)
            } else { None };
            let mut direct_channels = if owner == f.corp && wallet == f.corp {
                let (actor_tx, actor_rx) = tokio::sync::mpsc::unbounded_channel();
                let (ceo_tx, ceo_rx) = tokio::sync::mpsc::unbounded_channel();
                client.world.register_pilot(f.actor, actor_tx);
                client.world.register_pilot(f.ceo, ceo_tx);
                Some((actor_rx, ceo_rx))
            } else { None };
            db.execute("UPDATE game.industry_jobs SET end_date=0 WHERE job_id=$1", &[&id]).await.unwrap();
            db.execute("UPDATE game.jobs SET due_at=0 WHERE job_id=$1", &[&deadline]).await.unwrap();
            let output_before = quantities(&db, owner, output.item_id, output.flag_id, product).await;
            if direct_channels.is_some() && activity == MANUFACTURING {
                let replay = crate::scheduler::Job { id: deadline.clone(), kind: completion::KIND.into(), due_at: 0, payload: id.to_string() };
                let handler = completion::handle(db.clone(), client.world.clone(), replay);
                let (delivered, handled) = tokio::join!(mechanics::complete(&mut client, id), handler);
                delivered.ok().unwrap();
                handled.unwrap();
            } else {
                mechanics::complete(&mut client, id).await.ok().unwrap();
            }
            if let Some((actor_rx, ceo_rx)) = direct_channels.as_mut() {
                for receiver in [actor_rx, ceo_rx] {
                    let mut statuses = Vec::new();
                    let mut blueprint_updates = 0;
                    while let Ok((event, args)) = receiver.try_recv() {
                        if event == "OnIndustryJob" {
                            assert_eq!(args.get(0).and_then(Value::as_int), Some(id));
                            statuses.push(args.get(4).and_then(Value::as_int).unwrap());
                        }
                        if event == "OnBlueprintsUpdated" { blueprint_updates += 1; }
                    }
                    assert_eq!(statuses.iter().filter(|&&status| status == i64::from(STATUS_DELIVERED)).count(), 1);
                    assert!(statuses.iter().filter(|&&status| status == i64::from(INDUSTRY_JOB_STATUS_READY)).count() <= 1);
                    if activity == COPYING {
                        assert_eq!(statuses, vec![i64::from(STATUS_DELIVERED)]);
                        assert_eq!(blueprint_updates, 1);
                    } else {
                        assert_eq!(blueprint_updates, statuses.len());
                    }
                }
            }
            if let Some((actor_rx, ceo_rx)) = recipients.as_mut() {
                for receiver in [actor_rx, ceo_rx] {
                    let (event, args) = receiver.try_recv().unwrap();
                    assert_eq!(event, "OnIndustryJob");
                    assert_eq!(args.get(0).and_then(Value::as_int), Some(id));
                    assert_eq!(args.get(4).and_then(Value::as_int), Some(i64::from(STATUS_DELIVERED)));
                    assert_eq!(receiver.try_recv().unwrap().0, "OnBlueprintsUpdated");
                    assert!(receiver.try_recv().is_err());
                }
            }
            assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap().is_none());
            assert!(!industry_dao::has_active_blueprint_job(&db, &[blueprint]).await.unwrap());
            let state = fingerprint(&db, f).await;
            completion::handle(db.clone(), client.world.clone(), crate::scheduler::Job { id: deadline.clone(), kind: completion::KIND.into(), due_at: 0, payload: id.to_string() }).await.unwrap();
            assert_eq!(fingerprint(&db, f).await, state);
            if owner == f.actor && wallet == f.actor {
                assert!(!industry_dao::has_active_blueprint_job(&db, &[blueprint, f.office]).await.unwrap());
                let container = db.query_one("SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id = ANY($3) ORDER BY item_id DESC LIMIT 1", &[&f.actor, &f.station, &sde.types.iter().filter(|t| t.group_id.is_some_and(|g| CARGO_CONTAINER_GROUPS.contains(&g)) && sde.is_tradable(t.type_id)).map(|t| t.type_id).collect::<Vec<_>>()]).await.unwrap().get("item_id");
                let destination = crate::services::inventory::access::Container::Item(container);
                assert_eq!(crate::services::inventory::movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, blueprint, Some(f.station), Some(FLAG_CARGO), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.ok().unwrap(), Some(blueprint));
            }
            let output_after = quantities(&db, owner, output.item_id, output.flag_id, product).await;
            if activity == MANUFACTURING { assert_eq!(output_after - output_before, act.products[0].1); }
            else {
                assert_eq!(output_after - output_before, BLUEPRINT_COPY_QUANTITY);
                let row = db.query_one("SELECT b.runs, i.quantity, i.singleton FROM game.blueprints b JOIN game.items i ON i.item_id=b.item_id WHERE i.owner_id=$1 AND i.location_id=$2 AND i.flag_id=$3 AND i.type_id=$4 ORDER BY i.item_id DESC LIMIT 1", &[&owner, &output.item_id, &output.flag_id, &f.bp]).await.unwrap();
                assert_eq!(row.get::<_, i32>("runs"), 1);
                assert_eq!(i64::from(row.get::<_, i32>("quantity")), BLUEPRINT_COPY_QUANTITY);
                assert!(row.get::<_, bool>("singleton"));
            }
            let state = fingerprint(&db, f).await;
            assert!(mechanics::complete(&mut client, id).await.is_err());
            assert_eq!(fingerprint(&db, f).await, state);
        }
        let missing = i64::MAX;
        assert!(industry_dao::job(&db, missing).await.unwrap().is_none());
        let before = fingerprint(&db, f).await;
        completion::handle(db.clone(), client.world.clone(), crate::scheduler::Job { id: format!("industry.ready:{missing}"), kind: completion::KIND.into(), due_at: 0, payload: missing.to_string() }).await.unwrap();
        assert_eq!(fingerprint(&db, f).await, before);
        readiness::science_readiness(&db, &mut client, f, &personal, &sde, &prices).await;
        let act = sde.industry.activity(f.bp, MANUFACTURING).unwrap();
        let blueprint = item(&db, f.bp, f.corp, f.office, FLAG_CORP_SAG1, 1, true).await;
        for &(kind, quantity) in &act.materials {
            item(&db, kind, f.corp, f.office, FLAG_CORP_SAG1, i32::try_from(quantity + 10).unwrap(), false).await;
        }
        let valid = request(f, blueprint, MANUFACTURING, &divisions[0], &divisions[1], f.corp, ACCOUNT_CASH + 1);
        for field in ["cost", "tax"] {
            let mut stale = valid.clone();
            let Value::Dict(pairs) = &mut stale else { unreachable!() };
            pairs.push((Value::str(field), Value::Float(-1.0)));
            client.outbox.clear();
            let state = fingerprint(&db, f).await;
            assert!(mechanics::install(&mut client, &stale).await.is_err());
            assert_eq!(fingerprint(&db, f).await, state);
            assert!(client.outbox.is_empty());
        }
        let foreign_bp = item(&db, f.bp, f.ceo, f.office, FLAG_CORP_SAG1, 1, true).await;
        let misplaced_bp = item(&db, f.bp, f.corp, f.station, FLAG_HANGAR, 1, true).await;
        for bad in [
            request(f, foreign_bp, MANUFACTURING, &divisions[0], &divisions[1], f.corp, ACCOUNT_CASH + 1),
            request(f, misplaced_bp, MANUFACTURING, &divisions[0], &divisions[1], f.corp, ACCOUNT_CASH + 1),
            request(f, blueprint, MANUFACTURING, &divisions[0], &divisions[1], f.corp, ACCOUNT_CASH + 2),
        ] {
            client.outbox.clear();
            let state = fingerprint(&db, f).await;
            assert!(mechanics::install(&mut client, &bad).await.is_err());
            assert_eq!(fingerprint(&db, f).await, state);
            assert!(client.outbox.is_empty());
        }
        db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&f.actor]).await.unwrap();
        client.outbox.clear();
        let state = fingerprint(&db, f).await;
        assert!(mechanics::install(&mut client, &valid).await.is_err());
        assert_eq!(fingerprint(&db, f).await, state);
        assert!(client.outbox.is_empty());
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&f.actor, &roles]).await.unwrap();
        let first_material = act.materials[0].0;
        db.execute("DELETE FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.corp, &f.office, &FLAG_CORP_SAG1, &first_material]).await.unwrap();
        client.outbox.clear();
        let state = fingerprint(&db, f).await;
        assert!(mechanics::install(&mut client, &valid).await.is_err());
        assert_eq!(fingerprint(&db, f).await, state);
        assert!(client.outbox.is_empty());
        let first_quantity = act.materials[0].1;
        item(&db, first_material, f.corp, f.office, FLAG_CORP_SAG1, i32::try_from(first_quantity + 10).unwrap(), false).await;
        db.execute("UPDATE game.corp_wallets SET balance=0 WHERE corporation_id=$1 AND account_key=$2", &[&f.corp, &(ACCOUNT_CASH + 1)]).await.unwrap();
        client.outbox.clear();
        let state = fingerprint(&db, f).await;
        assert!(mechanics::install(&mut client, &valid).await.is_err());
        assert_eq!(fingerprint(&db, f).await, state);
        assert!(client.outbox.is_empty());
        db.execute("UPDATE game.corp_wallets SET balance=1000000000 WHERE corporation_id=$1 AND account_key=$2", &[&f.corp, &(ACCOUNT_CASH + 1)]).await.unwrap();
        for mutation in 0..3 {
            let bp = item(&db, f.bp, f.corp, f.office, FLAG_CORP_SAG1, 1, true).await;
            for &(kind, quantity) in &act.materials {
                item(&db, kind, f.corp, f.office, FLAG_CORP_SAG1, i32::try_from(quantity + 10).unwrap(), false).await;
            }
            let req = request(f, bp, MANUFACTURING, &divisions[0], &divisions[1], f.corp, ACCOUNT_CASH + 1);
            let id = mechanics::install(&mut client, &req).await.ok().unwrap().as_int().unwrap();
            db.execute("UPDATE game.industry_jobs SET end_date=0 WHERE job_id=$1", &[&id]).await.unwrap();
            let deadline = format!("industry.ready:{id}");
            db.execute("UPDATE game.jobs SET due_at=0 WHERE job_id=$1", &[&deadline]).await.unwrap();
            match mutation {
                0 => { db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&bp, &f.actor]).await.unwrap(); }
                1 => { db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&bp, &f.station, &FLAG_HANGAR]).await.unwrap(); }
                _ => { db.execute("DELETE FROM game.items WHERE item_id=$1", &[&bp]).await.unwrap(); }
            }
            client.outbox.clear();
            let state = fingerprint(&db, f).await;
            assert!(mechanics::complete(&mut client, id).await.is_err());
            assert_eq!(fingerprint(&db, f).await, state);
            assert!(client.outbox.is_empty());
            assert_eq!(industry_dao::job(&db, id).await.unwrap().unwrap().status, INDUSTRY_JOB_STATUS_INSTALLED);
            let args = [Value::Int(id), Value::Int(f.system)];
            let kwargs = Value::None;
            let call = Call { service: "industryManager", method: "CancelJob", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert!(industry_manager::dispatch(&mut client, &call).await.unwrap().is_ok());
            assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap().is_none());
            let cancelled = fingerprint(&db, f).await;
            completion::handle(db.clone(), client.world.clone(), crate::scheduler::Job { id: deadline, kind: completion::KIND.into(), due_at: 0, payload: id.to_string() }).await.unwrap();
            assert_eq!(fingerprint(&db, f).await, cancelled);
            if mutation == 2 {
                db.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,1,true)", &[&bp, &f.bp, &f.corp, &f.office, &FLAG_CORP_SAG1]).await.unwrap();
            } else {
                db.execute("UPDATE game.items SET owner_id=$2,location_id=$3,flag_id=$4 WHERE item_id=$1", &[&bp, &f.corp, &f.office, &FLAG_CORP_SAG1]).await.unwrap();
            }
        }
        for &(kind, quantity) in &act.materials {
            item(&db, kind, f.corp, f.office, FLAG_CORP_SAG1, i32::try_from(quantity + 10).unwrap(), false).await;
        }
        let id = mechanics::install(&mut client, &valid).await.ok().unwrap().as_int().unwrap();
        db.execute("UPDATE game.industry_jobs SET end_date=0 WHERE job_id=$1", &[&id]).await.unwrap();
        let deadline = format!("industry.ready:{id}");
        db.execute("UPDATE game.jobs SET due_at=0 WHERE job_id=$1", &[&deadline]).await.unwrap();
        db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&f.actor]).await.unwrap();
        client.outbox.clear();
        let state = fingerprint(&db, f).await;
        assert!(mechanics::complete(&mut client, id).await.is_err());
        let args = [Value::Int(id), Value::Int(f.system)];
        let kwargs = Value::None;
        let call = Call { service: "industryManager", method: "CancelJob", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(industry_manager::dispatch(&mut client, &call).await.unwrap().is_err());
        assert_eq!(fingerprint(&db, f).await, state);
        assert!(client.outbox.is_empty());
    }).await;
    db.execute("DELETE FROM game.jobs", &[]).await.unwrap();
    db.execute("INSERT INTO game.jobs SELECT (json_populate_record(NULL::game.jobs, value)).* FROM json_array_elements($1::text::json)", &[&saved_jobs]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id IN (SELECT 'trade-timeout:' || trade_id::text FROM game.trades WHERE char_a=$1 OR char_b=$1)", &[&f.actor]).await.unwrap();
    db.execute("DELETE FROM game.trade_items WHERE trade_id IN (SELECT trade_id FROM game.trades WHERE char_a=$1 OR char_b=$1)", &[&f.actor]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE char_a=$1 OR char_b=$1", &[&f.actor]).await.unwrap();
    db.execute("DELETE FROM game.market_transactions WHERE buyer_id=$1 OR seller_id=$1", &[&f.actor]).await.unwrap();
    db.execute("DELETE FROM game.market_order_history WHERE char_id=$1", &[&f.actor]).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE char_id=$1", &[&f.actor]).await.unwrap();
    db.execute("DELETE FROM game.industry_jobs WHERE installer_id=$1", &[&f.actor]).await.unwrap();
    db.execute("DELETE FROM game.blueprints WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id = ANY($1))", &[&vec![f.actor, f.ceo, f.corp]]).await.unwrap();
    db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&f.office]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id = ANY($1)", &[&vec![f.actor, f.ceo, f.corp]]).await.unwrap();
    db.execute("DELETE FROM game.skills WHERE character_id = ANY($1)", &[&vec![f.actor, f.ceo]]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&f.corp]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&f.corp]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id=$1", &[&f.account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&f.account]).await.unwrap();
    assert_eq!(super::database_fingerprint(&db).await, baseline);
    result.unwrap();
}
