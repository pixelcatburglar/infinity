//! Historical same-pilot settlement through the production claim transaction.
//! The endpoint is trusted synthetic input, not an Entosis scoring or warmup test.
//! Corporation changes stage fixture facts, not the public employment workflow.
use std::{sync::{Arc, Mutex}, time::Duration};
use marshal::{DbRowDescriptor, Value};
use crate::{
    client::{filetime_from_unix_ms, filetime_now},
    dao::{corporations::registry, items::space_ships::FreeShip, mail::{self, NotificationFilter}, skills::training, sovereignty::{capture, history, policy, sources, structures, systems}},
    db::outbox::{dispatch_world, Outbox},
    services::{self, Call, sovereignty::{self, deploy::{self, DeployRequest}, lifecycle::{self, CaptureOutcome}, queries::{self, Query}, CaptureContribution, CaptureContributor, CaptureObservation}},
};
use super::fixture::{self, Fixture};

#[derive(Clone, Copy, Default)]
struct Extra { character: i64, ship: i64, module: i64 }

async fn seed(f: &Fixture, extra: Arc<Mutex<Extra>>) -> Extra {
    let db = &f.resources.db;
    let row = db.query_one("SELECT nextval('game.characters_character_id_seq') AS character, nextval('game.items_item_id_seq') AS ship, nextval('game.items_item_id_seq') AS module", &[]).await.unwrap();
    let ids = Extra { character: row.get("character"), ship: row.get("ship"), module: row.get("module") };
    *extra.lock().unwrap() = ids;
    let original = f.ids;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,corp_roles,ship_id,station_id,solar_system_id,constellation_id,region_id) SELECT $1,user_id,'hub-historical-' || $1::bigint::text,race_id,bloodline_id,gender_id,corporation_id,0,$2,NULL,solar_system_id,constellation_id,region_id FROM game.characters WHERE character_id=$3", &[&ids.character, &ids.ship, &original.characters[0]]).await?;
        tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) SELECT $1,type_id,$2,location_id,flag_id,quantity,singleton,x,y,z FROM game.items WHERE item_id=$3", &[&ids.ship, &ids.character, &original.ships[0]]).await?;
        tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,online) SELECT $1,type_id,$2,$3,flag_id,quantity,singleton,online FROM game.items WHERE item_id=$4", &[&ids.module, &ids.character, &ids.ship, &original.modules[0]]).await?;
        for skill in training::current(tx, original.characters[0]).await? { training::starting_skill(tx, ids.character, skill.type_id, skill.level, skill.skill_points).await?; }
        sources::ensure(tx, ids.character).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
    let row = db.query_one("SELECT type_id,x,y,z FROM game.items WHERE item_id=$1", &[&ids.ship]).await.unwrap();
    let ship = FreeShip { item_id: ids.ship, type_id: row.get("type_id"), owner_id: ids.character, corporation_id: original.corps[0], alliance_id: original.alliances[0], pos: [row.get("x"), row.get("y"), row.get("z")] };
    let prepared = crate::services::ship::balls::piloted(db, f.resources.sde.clone(), &ship, ids.character, 0.0).await.unwrap();
    f.resources.world.with(original.systems[0], move |system| prepared.install(system, |s, ball| s.add_ship(ball))).await.unwrap();
    ids
}

async fn cleanup_extra(ids: Extra) {
    if ids.character == 0 { return; }
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.notifications WHERE receiver_id=$1", &[&ids.character]).await?;
        tx.execute("DELETE FROM game.sov_link_source_changes WHERE character_id=$1", &[&ids.character]).await?;
        tx.execute("DELETE FROM game.sov_link_sources WHERE character_id=$1", &[&ids.character]).await?;
        tx.execute("DELETE FROM game.employment_history WHERE char_id=$1", &[&ids.character]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&ids.character]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&&[ids.ship, ids.module][..]]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&ids.character]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
}

async fn rpc(client: &mut crate::client::Client, service: &str, method: &str, args: &[Value]) -> Value {
    let kwargs = Value::None;
    let call = Call { service, method, args, kwargs: &kwargs, bound: false, bind_params: None };
    services::dispatch(client, &call).await.unwrap()
}

async fn projections(f: &Fixture, client: &mut crate::client::Client, hub: i64, claimed: bool) {
    let ids = f.ids;
    let r = &f.resources;
    let system = ids.systems[0];
    let expected_alliance = claimed.then_some(ids.alliances[0]).map_or(Value::None, Value::Int);
    let expected_corp = claimed.then_some(ids.corps[1]).map_or(Value::None, Value::Int);
    let map = rpc(client, "map", "GetCurrentSovData", &[Value::Int(system)]).await;
    assert_eq!(services::value_field(&map, "header"), Some(&Value::list([Value::str("locationID"), Value::str("corporationID"), Value::str("allianceID"), Value::str("claimTime")])));
    let rowset = services::value_field(&map, "lines").unwrap().items().unwrap();
    assert_eq!(rowset.len(), 1);
    let row = rowset[0].items().unwrap();
    assert_eq!(&row[..3], &[Value::Int(system), expected_corp.clone(), expected_alliance.clone()]);
    assert_eq!(row[3].as_int().is_some(), claimed);
    let station = rpc(client, "stationSvc", "GetAllianceSystems", &[]).await;
    assert_eq!(services::value_field(&station, "header"), Some(&Value::list([Value::str("solarSystemID"), Value::str("allianceID")])));
    assert_eq!(services::value_field(&station, "lines"), Some(&if claimed { Value::list([Value::list([Value::Int(system), expected_alliance.clone()])]) } else { Value::list([]) }));
    let alliance = rpc(client, "allianceRegistry", "GetAlliance", &[Value::Int(ids.alliances[0])]).await;
    let Value::DbRow { descriptor, .. } = &alliance else { panic!("expected alliance DBRow") };
    let descriptor = DbRowDescriptor::from_value(descriptor).unwrap();
    assert_eq!(descriptor.get(&alliance, "currentCapital"), Some(if claimed { Value::Int(system) } else { Value::None }));
    let capital = queries::read(&r.db, &r.sde, Query::CapitalSystemInfo { alliance_id: ids.alliances[0] }).await.unwrap();
    assert_eq!(services::value_field(&capital, "currentCapitalSystem"), Some(&if claimed { Value::Int(system) } else { Value::None }));
    for field in ["newCapitalSystem", "newCapitalSystemValidAfter"] { assert_eq!(services::value_field(&capital, field), Some(&Value::None)); }
    let legacy = queries::read(&r.db, &r.sde, Query::SystemSovereigntyInfo { system_id: system }).await.unwrap();
    let legacy_hub = queries::read(&r.db, &r.sde, Query::InfrastructureHubInfo { system_id: system }).await.unwrap();
    assert_eq!(legacy == Value::None, !claimed);
    assert_eq!(legacy_hub == Value::None, !claimed);
    let list = queries::read(&r.db, &r.sde, Query::Structures { system_id: system }).await.unwrap();
    let rows = list.items().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(services::value_field(&rows[0], "itemID"), Some(&Value::Int(hub)));
    assert_eq!(services::value_field(&rows[0], "allianceID"), Some(&expected_alliance));
    assert_eq!(services::value_field(&rows[0], "isCapital"), Some(&Value::Bool(claimed)));
    let dual = queries::read(&r.db, &r.sde, Query::AllianceStructures { alliance_id: ids.alliances[0] }).await.unwrap();
    let parts = dual.items().unwrap();
    assert_eq!(parts[0], parts[1]);
    let Value::Reduce { data, .. } = &parts[0] else { panic!("expected CRowset") };
    let args = data.items().unwrap()[1].items().unwrap();
    let descriptor = DbRowDescriptor::from_value(&args[0]).unwrap();
    let rows = args[1].items().unwrap();
    assert_eq!(rows.len(), usize::from(claimed));
    if claimed { assert_eq!(descriptor.get(&rows[0], "structureID"), Some(Value::Int(hub))); assert_eq!(descriptor.get(&rows[0], "corporationID"), Some(Value::Int(ids.corps[1]))); }
}

async fn run(f: &Fixture, extra: Arc<Mutex<Extra>>) {
    let ids = f.ids;
    let r = &f.resources;
    let hub = deploy::deploy(r.clone(), f.identity(0), DeployRequest { source_item_id: ids.sources[0], quantity: 1, whose_behalf: None }).await.unwrap().item_id;
    let pilot = seed(f, extra).await;
    let (mut client, _peer) = services::tests::client(r.db.clone(), pilot.character).await;
    client.login.user_id = ids.account;
    projections(f, &mut client, hub, false).await;
    let system = systems::read(&r.db, ids.systems[0]).await.unwrap().unwrap();
    let mut target = capture::for_system(&r.db, ids.systems[0]).await.unwrap().unwrap();
    let prefix = loop {
        let observed = r.world.with(ids.systems[0], |s| filetime_from_unix_ms(s.now_ms())).await.unwrap();
        if observed >= target.score_sim_time { break observed; }
        tokio::task::yield_now().await;
    };
    // Both versions and membership are read while holding the same admission guard.
    let old = r.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(registry::lock_admission(tx, &[ids.alliances[0]], &ids.corps[..2], &[pilot.character]).await?);
        assert_eq!(registry::membership(tx, pilot.character).await?.corporation_id, ids.corps[0]);
        Ok::<_, crate::db::Error>(sources::versions(tx, &[pilot.character]).await?.remove(0).revision)
    }), |v, ()| async move { Ok(v) }).await.unwrap();
    assert_eq!(old, 0);
    let before = CaptureContributor { character_id: pilot.character, ship_id: pilot.ship, module_id: pilot.module, corporation_id: ids.corps[0], alliance_id: ids.alliances[0], source_revision: old };
    r.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        assert!(registry::lock_admission(tx, &[ids.alliances[0]], &ids.corps[..2], &[pilot.character]).await.map_err(services::db_err)?);
        assert!(registry::set_corp(tx, pilot.character, ids.corps[1], [0; 4], filetime_now()).await.map_err(services::db_err)?);
        sovereignty::invalidate_sources(tx, outbox, &[pilot.character]).await?;
        Ok::<_, services::CallError>(())
    }), |value, outbox| async move { dispatch_world(value, outbox, &r.world) }).await.unwrap();
    let change = sources::first_change_after(&r.db, pilot.character, old).await.unwrap().unwrap();
    assert!(prefix <= change.effective_at);
    let revision = sources::versions(&r.db, &[pilot.character]).await.unwrap().remove(0).revision;
    assert_eq!(revision, old + 1);
    assert_eq!(registry::membership(&r.db, pilot.character).await.unwrap().corporation_id, ids.corps[1]);
    let endpoint = loop {
        let observed = r.world.with(ids.systems[0], |s| filetime_from_unix_ms(s.now_ms())).await.unwrap();
        if observed >= change.effective_at && observed >= prefix { break observed; }
        tokio::task::yield_now().await;
    };
    let current = CaptureContributor { corporation_id: ids.corps[1], source_revision: revision, ..before.clone() };
    target.controlling_team = Some(ids.alliances[0]); target.scoring_team = Some(ids.alliances[0]);
    target.score = 1.0; target.rate = 1.0; target.blocked = false; target.score_sim_time = endpoint;
    let observation = CaptureObservation { target, system_revision: system.revision, contributors: vec![CaptureContribution { contributor: before, through: prefix }, CaptureContribution { contributor: current.clone(), through: endpoint }], claimant: Some(current) };
    assert!(matches!(lifecycle::claim(r.clone(), observation).await.unwrap(), CaptureOutcome::Claimed));
    let snapshot = structures::snapshots(&r.db, Some(ids.systems[0]), None).await.unwrap().remove(0);
    assert_eq!(snapshot.owner_id, ids.corps[1]);
    assert_eq!(snapshot.system.claim_corporation_id, Some(ids.corps[1]));
    assert_eq!(snapshot.system.alliance_id, Some(ids.alliances[0]));
    assert_eq!(policy::read(&r.db, ids.alliances[0]).await.unwrap().unwrap().current_capital_system, Some(ids.systems[0]));
    let rows = history::recent(&r.db).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].old_alliance_id, rows[0].new_alliance_id, rows[0].new_corporation_id), (None, Some(ids.alliances[0]), Some(ids.corps[1])));
    let notices = mail::notifications(&r.db, pilot.character, NotificationFilter::After, 0).await.unwrap();
    assert!(notices.iter().any(|n| n.type_id == 44 && serde_json::from_str::<serde_json::Value>(&n.data).unwrap() == serde_json::json!({"solarSystemID":ids.systems[0],"corpID":ids.corps[1],"allianceID":ids.alliances[0]})));
    projections(f, &mut client, hub, true).await;
    let claim_history = rows[0].clone();
    let position = [snapshot.x.unwrap(), snapshot.y.unwrap(), snapshot.z.unwrap()];
    f.install_pilot(1, 0, position).await;
    lifecycle::transfer(r.clone(), f.identity(1), hub, ids.corps[1], ids.corps[0]).await.unwrap();
    assert_eq!(history::recent(&r.db).await.unwrap().into_iter().find(|row| row.operation_key == claim_history.operation_key), Some(claim_history.clone()));
    assert_eq!(history::recent(&r.db).await.unwrap(), vec![claim_history]);
    let map_history = rpc(&mut client, "map", "GetRecentSovActivity", &[]).await;
    assert_eq!(services::value_field(&map_history, "lines"), Some(&Value::list([Value::list([Value::Int(ids.systems[0]), Value::None, Value::None, Value::Int(ids.alliances[0])])])));
}

#[test]
#[ignore = "requires a dedicated initialized INFINITY_TEST_DATABASE_URL and real SDE; run this exact test in its own process"]
fn historical_same_pilot_corporation_claim_integration() {
    crate::tests::init_config();
    let state = Arc::new(Mutex::new(fixture::Recorded::default()));
    let extra = Arc::new(Mutex::new(Extra::default()));
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let shared = state.clone();
    let extra_task = extra.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(async {
        let mut task = tokio::spawn(async move { let fixture = Fixture::new(shared).await; run(&fixture, extra_task).await; });
        let result = tokio::time::timeout(Duration::from_secs(240), &mut task).await;
        if result.is_err() { task.abort(); let _ = task.await; }
        let world = state.lock().unwrap().world.clone();
        if let Some(world) = world { crate::space::sovereignty::close(&world); }
        let _fence = services::market::lock().await;
        match result { Ok(Ok(())) => {}, Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()), Ok(Err(error)) => panic!("integration task cancelled: {error}"), Err(_) => panic!("historical claim scenario timed out") }
    })));
    drop(runtime);
    let ids = state.lock().unwrap().ids;
    let extra = *extra.lock().unwrap();
    let cleanup_runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup_runtime.block_on(async { cleanup_extra(extra).await; fixture::cleanup(ids).await; })));
    if let Err(error) = cleanup { eprintln!("historical fixture cleanup failed: {error:?}"); if let Err(original) = result { std::panic::resume_unwind(original); } std::panic::resume_unwind(error); }
    if let Err(error) = result { std::panic::resume_unwind(error); }
}
