use crate::client::Client;
use infinity_wire::conn::Conn;
use crate::handshake::Login;
use marshal::Value;
use std::sync::Arc;
mod organization;
mod skill_plan_recovery;
mod boarding_receipt;
mod bay_receipt;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with initialized SDE"]
async fn session_changes_keep_channel_and_complete_chat_metadata() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(crate::config::Config::default());
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let mut client = Client::new(Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    let (original_tx, mut original_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = original_tx;
    client.change_session(vec![
        ("charid", Value::Int(1)), ("corpid", Value::Int(2)),
        ("solarsystemid2", Value::Int(3)), ("allianceid", Value::Int(4)),
        ("warfactionid", Value::Int(5)), ("fleetid", Value::Int(6)),
        ("role", Value::Int(7)), ("corprole", Value::Int(8)),
    ]);
    let snapshot = world.online_session(1).unwrap();
    assert_eq!((snapshot.system_id, snapshot.corporation_id, snapshot.alliance_id), (Some(3), Some(2), Some(4)));
    assert_eq!((snapshot.faction_id, snapshot.fleet_id, snapshot.role, snapshot.corporation_role), (Some(5), Some(6), 7, 8));
    let (replacement_tx, mut replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = replacement_tx;
    client.change_session(vec![("solarsystemid2", Value::Int(9))]);
    let updated = world.online_session(1).unwrap();
    assert_eq!(updated.system_id, Some(9));
    assert_eq!((updated.alliance_id, updated.faction_id, updated.fleet_id, updated.role, updated.corporation_role), (Some(4), Some(5), Some(6), 7, 8));
    world.notify_pilot(1, "test", vec![]);
    assert_eq!(original_rx.try_recv().unwrap().0, "test");
    assert!(replacement_rx.try_recv().is_err());
    client.change_session(vec![("charid", Value::Int(10))]);
    assert!(!world.is_online(1));
    assert!(world.online_session(1).is_none());
    assert_eq!(world.online_session(10).unwrap().character_id, 10);
    world.notify_pilot(10, "replacement", vec![]);
    assert_eq!(replacement_rx.try_recv().unwrap().0, "replacement");
    client.change_session(vec![("charid", Value::None)]);
    assert!(!world.is_online(10));
    assert!(world.online_session(10).is_none());
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn loss_between_selection_and_publication_cannot_restore_destroyed_ship() {
    use infinity_core::consts::CATEGORY_SHIP;
    use crate::{dao::{characters, items}, services::{combat, ship}, space::World};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.type_id != infinity_core::consts::TYPE_CAPSULE && t.base_price.is_some_and(|v| v > 1.0)).unwrap().type_id;
    let system_row = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let (system, constellation, region) = (system_row.solar_system_id, system_row.constellation_id.unwrap(), system_row.region_id.unwrap());
    let home = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap().station_id;
    let (account, character, vessel) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,home_station_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,NULL,$3,$4,$5) RETURNING character_id", &[&account, &home, &system, &constellation, &region]).await?.get("character_id");
        let vessel = items::creation::positioned(tx, ship_type, character, system, [0.0; 3]).await?;
        Ok::<_, crate::db::Error>((account, character, vessel))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let result = crate::tests::catch_scenario(async {
    characters::selection::set_ship(&db, character, vessel).await.unwrap();
    let selected = characters::selection::selected(&db, character, account).await.unwrap().unwrap();
    let now = crate::client::filetime_now();
    crate::dao::insurance::insure(&db, vessel, character, ship_type, 1.0, now, now + 3600 * infinity_core::consts::FILETIME_SECOND).await.unwrap();
    let balance_before = crate::services::wallets::balance(&db, character).await.ok().unwrap();
    let ids = [infinity_core::consts::dogma::ATTR_SHIELD_CHARGE, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, infinity_core::consts::dogma::ATTR_HULL_DAMAGE];
    assert_eq!(items::dogma::save_conditions(&db, system, &ids, &[(vessel, [0.0, 0.0, 1e30], now)]).await.unwrap(), vec![vessel]);
    let world = World::new(db.clone(), sde.clone(), 1);
    let row = items::space_ships::FreeShip { item_id: vessel, type_id: ship_type, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
    let prepared = ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.system(system).await.unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    let outcome = combat::damage::apply_damage_guarded(&db, &world, system, vessel, [0.0; 4],
        combat::damage::DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap();
    assert!(outcome.lethal);
    let capsule = characters::selection::ship_id(&db, character).await.unwrap().unwrap();
    assert!(crate::dao::insurance::contract(&db, vessel).await.unwrap().is_none());
    let balance_after = crate::services::wallets::balance(&db, character).await.ok().unwrap();
    assert!(balance_after > balance_before);
    let again = combat::damage::apply_damage_guarded(&db, &world, system, vessel, [0.0; 4],
        combat::damage::DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap();
    assert!(again.is_none());
    assert_eq!(crate::services::wallets::balance(&db, character).await.ok().unwrap(), balance_after);
    assert_ne!(capsule, vessel);
    let admission = world.admission(system).unwrap();
    assert!(super::publish_selected_ship(&db, &world, &sde, character, vessel, &selected, admission).await.is_err());
    assert!(world.ball(system, vessel).await.is_none());
    assert!(world.with(system, move |s| s.ship_condition(vessel)).await.flatten().is_none());
    assert_eq!(characters::selection::ship_id(&db, character).await.unwrap(), Some(capsule));
    assert_eq!(db.query("SELECT kill_id FROM game.killmails WHERE victim_item_id=$1", &[&vessel]).await.unwrap().len(), 1);
    assert_eq!(db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&character, &infinity_core::consts::TYPE_CAPSULE]).await.unwrap().len(), 1);
    world.remove_ball(system, capsule).await;
    }).await;
    db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id IN (SELECT kill_id FROM game.killmails WHERE victim_item_id=$2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &vessel]).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_recovery(tx, character).await?;
        crate::services::tests::fixture::cleanup_progress(tx, &[account, character], &[character]).await?;
        let kills: Vec<i64> = tx.query("SELECT kill_id FROM game.killmails WHERE victim_item_id=$1", &[&vessel]).await?.iter().map(|row| row.get("kill_id")).collect();
        crate::services::tests::fixture::cleanup_killmail_rewards(tx, &kills).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.killmails WHERE victim_item_id=$1", &[&vessel]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn station_presence_sends_one_record_to_correct_peers() {
    use infinity_core::session::OnlineSession;
    use crate::services::identity::Identity;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db, 1).await;
    let world = &client.world;
    let chat = &client.chat;
    let session = |id, station| OnlineSession {
        character_id: id, user_id: None, session_id: 0, ship_id: None, station_id: station, system_id: None, corporation_id: Some(7),
        alliance_id: None, faction_id: Some(8), fleet_id: None, role: 0, corporation_role: 0,
    };
    let identity = Identity { char_id: 1, corp_id: 7, ship_id: None, station_id: None, system_id: None };
    let (a_tx, mut a_rx) = tokio::sync::mpsc::unbounded_channel();
    let (b_tx, mut b_rx) = tokio::sync::mpsc::unbounded_channel();
    let (other_tx, mut other_rx) = tokio::sync::mpsc::unbounded_channel();
    for (id, station, tx) in [(2, 10, a_tx), (3, 20, b_tx), (4, 30, other_tx)] {
        world.register_pilot(id, tx);
        world.set_online_session(id, Some(session(id, Some(station))));
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let docked = session(1, Some(10));
    super::online(&identity, world, chat, tx, None, docked.clone());
    let record = Value::tuple([Value::Int(1), Value::Int(7), Value::None, Value::Int(8)]);
    assert_eq!(a_rx.try_recv().unwrap(), ("OnCharNowInStation".into(), vec![record.clone()]));
    assert!(b_rx.try_recv().is_err());
    let (replacement, mut replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    super::online(&identity, world, chat, replacement.clone(), Some(&docked), docked.clone());
    assert!(a_rx.try_recv().is_err());
    let moved = session(1, Some(20));
    super::online(&identity, world, chat, replacement.clone(), Some(&docked), moved.clone());
    assert_eq!(a_rx.try_recv().unwrap(), ("OnCharNoLongerInStation".into(), vec![record.clone()]));
    assert_eq!(b_rx.try_recv().unwrap(), ("OnCharNowInStation".into(), vec![record.clone()]));
    let space = session(1, None);
    super::online(&identity, world, chat, replacement.clone(), Some(&moved), space.clone());
    assert_eq!(b_rx.try_recv().unwrap(), ("OnCharNoLongerInStation".into(), vec![record.clone()]));
    super::online(&identity, world, chat, replacement, Some(&space), moved);
    assert_eq!(b_rx.try_recv().unwrap(), ("OnCharNowInStation".into(), vec![record.clone()]));
    world.notify_pilot(1, "channel", vec![]);
    assert_eq!(rx.try_recv().unwrap().0, "channel");
    assert!(replacement_rx.try_recv().is_err());
    super::offline_generation(&identity, world, chat, Some(0));
    assert_eq!(b_rx.try_recv().unwrap(), ("OnCharNoLongerInStation".into(), vec![record]));
    assert!(other_rx.try_recv().is_err());
    assert!(a_rx.try_recv().is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn prepare_restores_docked_and_space_pilots_without_a_connection() {
    use infinity_core::{consts::{FLAG_HANGAR, TYPE_CAPSULE}};
    use crate::{config::deployment::DatabaseConfig, dao::{characters, items}, db::Db, sde::Sde, space::{World, CLOAK_RESTORE}};
    use crate::services::fit::{Key, attributes::CommittedFitting};
    use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_CHARGE, CATEGORY_SHIP};
    use infinity_core::consts::dogma::{ATTR_CAPACITY, ATTR_MAX_VELOCITY};
    use crate::services::effects::{self, projection::Projection, EffectKind, JammingType};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let system_row = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let (system, constellation, region) = (system_row.solar_system_id, system_row.constellation_id.unwrap(), system_row.region_id.unwrap());
    let (account, docked, space) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let docked: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,$3,$4,$5) RETURNING character_id", &[&account, &station, &system, &constellation, &region]).await?.get("character_id");
        let space: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4) RETURNING character_id", &[&account, &system, &constellation, &region]).await?.get("character_id");
        Ok::<_, crate::db::Error>((account, docked, space))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let test_world = world.clone();
    let mut recipient = None;
    let result = crate::tests::catch_scenario(async {
        let pos = [1234.0, 5678.0, 9012.0];
        let size = sde.dogma.attributes_by_name("chargeSize").next().map(|row| row.attribute_id);
        let (hull_type, module_type, charge_type, flag) = sde.types.iter()
            .filter(|row| row.published == Some(true) && row.category_id == Some(CATEGORY_MODULE))
            .find_map(|module| {
                let (_, slots, flag) = effects::fitting::rack(&sde.types, module.type_id)?;
                let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
                    .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|row| module.attribute(row.attribute_id)).collect();
                let module_size = size.and_then(|id| module.attribute(id));
                let capacity = module.capacity.or_else(|| module.attribute(ATTR_CAPACITY))?;
                let charge = sde.types.iter().find(|row| row.published == Some(true) && row.category_id == Some(CATEGORY_CHARGE)
                    && row.group_id.is_some_and(|group| groups.contains(&f64::from(group)))
                    && (module_size.is_none_or(|value| value <= 0.0) || size.and_then(|id| row.attribute(id)) == module_size)
                    && row.volume.is_some_and(|value| value > 0.0 && value.is_finite() && capacity.is_finite() && capacity >= value))?;
                let hull = sde.types.iter().find(|row| row.published == Some(true) && row.category_id == Some(CATEGORY_SHIP)
                    && row.type_id != TYPE_CAPSULE && row.attribute(slots).is_some_and(|value| value >= 1.0))?;
                Some((hull.type_id, module.type_id, charge.type_id, flag))
            }).expect("ship rack with compatible module and charge");
        let ship = items::creation::positioned(&test_db, hull_type, space, system, pos).await.unwrap();
        let module = items::creation::rookie_ship(&test_db, module_type, space, ship, flag).await.unwrap();
        test_db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,1)", &[&ship, &flag, &charge_type]).await.unwrap();
        characters::selection::set_ship(&test_db, space, ship).await.unwrap();
        let before = test_db.query("SELECT source, source_id FROM game.map_events WHERE solar_system_id=$1 ORDER BY source, source_id", &[&system]).await.unwrap()
            .iter().map(|r| (r.get::<_, i16>("source"), r.get::<_, i64>("source_id"))).collect::<Vec<_>>();
        let selected = characters::selection::selected(&test_db, docked, account).await.unwrap().unwrap();
        let prepared = super::prepare(&test_db, &test_world, &sde, docked, &selected, |n| n.dispatch(&test_world, None)).await.unwrap();
        assert_eq!(prepared.cloak_until, None);
        let hull = test_db.query_one("SELECT type_id, owner_id, location_id, flag_id FROM game.items WHERE item_id=$1", &[&prepared.ship_id]).await.unwrap();
        assert_eq!((hull.get::<_, i32>("type_id"), hull.get::<_, i64>("owner_id"), hull.get::<_, i64>("location_id"), hull.get::<_, i32>("flag_id")), (601, docked, station, FLAG_HANGAR));
        let mut fit = test_db.query("SELECT type_id, flag_id, quantity FROM game.items WHERE location_id=$1 AND owner_id=$2", &[&prepared.ship_id, &docked]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i32>("flag_id"), r.get::<_, i64>("quantity"))).collect::<Vec<_>>();
        fit.sort_unstable();
        let mut expected_fit = vec![(3638,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)];
        expected_fit.sort_unstable();
        assert_eq!(fit, expected_fit);
        let mut skills = test_db.query("SELECT type_id, level FROM game.skills WHERE character_id=$1", &[&docked]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i32>("level"))).collect::<Vec<_>>();
        let mut expected_skills = sde.characters.race(1).unwrap().skills.iter().map(|s| (s.type_id, s.level)).collect::<Vec<_>>();
        skills.sort_unstable();
        expected_skills.sort_unstable();
        assert_eq!(skills, expected_skills);
        let selected = characters::selection::selected(&test_db, space, account).await.unwrap().unwrap();
        {
            let market = crate::services::market::lock().await;
            recipient = Some(crate::services::tests::fixture::LiveRecipient::cold(&test_world, space, Some(system), &market).await);
        }
        let live = recipient.as_ref().unwrap();
        let drone = live.item;
        let (projection_type, projection_effect, modifier) = sde.types.iter()
            .filter(|row| row.category_id == Some(CATEGORY_MODULE))
            .find_map(|row| row.effects().iter().find_map(|entry| {
                let resolved = effects::registry::resolve(&sde, entry.effect_id)?;
                let effect = sde.dogma.effect(entry.effect_id)?;
                let modifier = effect.modifiers().iter().find(|m| m.modified == ATTR_MAX_VELOCITY)?;
                (resolved.kind == EffectKind::Projected { jamming: JammingType::Webify })
                    .then_some((row.type_id, entry.effect_id, modifier.modifying))
            })).expect("real projected velocity modifier");
        let own_before = test_world.with(live.system, move |s| {
            let generation = s.drone(drone).unwrap().generation;
            let membership = s.combat_membership_generation(drone).unwrap();
            let patch = CommittedFitting::overridden_attributes(vec![(Key::Drone(drone), ATTR_CAPACITY, Some(123.0))]);
            assert!(s.publish_drone_fitting(drone, space, generation, membership, patch));
            s.upsert_projection(drone, Projection {
                source_ship: drone, source_module: drone, source_type: projection_type, effect_id: projection_effect,
                kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX,
                values: vec![(modifier, -10.0)], charge_values: vec![], charge_type: None, resistance_attribute: None, scramble: None,
            });
            assert!(s.set_condition_fraction_mask(drone, 0b110, 0.75));
            let condition = s.combat_condition_snapshot(drone).unwrap().condition;
            assert!(condition.stats.max[1] > 0.0 && condition.stats.max[2] > 0.0);
            (condition.stats.max[1] - condition.now[1], condition.stats.max[2] - condition.now[2])
        }).await.unwrap();
        let own_state = move |s: &mut crate::space::System| {
            let record = s.drone(drone).unwrap();
            let control = (record.owner, record.controller, record.controller_owner, record.generation, record.order.clone());
            let condition = s.combat_condition_snapshot(drone).unwrap().condition;
            let damage = (condition.stats.max[1] - condition.now[1], condition.stats.max[2] - condition.now[2]);
            let fit = s.with_combat_fit(drone, move |fit, _, stamp| (
                stamp.membership, fit.source_type(Key::Drone(drone)), fit.value(Key::Drone(drone), ATTR_CAPACITY),
                fit.source_type(Key::Projected(drone)), fit.value(Key::Projected(drone), modifier),
            )).unwrap();
            (control, damage, fit)
        };
        let own = test_world.with(live.system, own_state).await.unwrap();
        assert_eq!(own.0.0, space);
        assert_eq!((own.0.1, own.0.2), (None, None));
        assert_eq!(own.1, own_before);
        assert!(own.2.1.is_some());
        assert_eq!(own.2.2, 123.0);
        assert_eq!(own.2.3, Some(projection_type));
        assert_eq!(own.2.4, -10.0);
        let start = crate::client::filetime_now();
        let prepared = super::prepare(&test_db, &test_world, &sde, space, &selected, |n| n.dispatch(&test_world, None)).await.unwrap();
        assert_eq!(prepared.ship_id, ship);
        assert_eq!(live.read(move |fit|
            (fit.owner_inputs().unwrap().ship, fit.source_type(Key::Ship), fit.source_type(Key::Module(module)), fit.source_type(Key::Charge(module)))).await,
            (ship, Some(hull_type), Some(module_type), Some(charge_type)));
        let after = test_world.with(live.system, own_state).await.unwrap();
        assert_eq!(after.0, own.0);
        assert_eq!(after.2, own.2);
        assert!((after.1.0 - own.1.0).abs() < 1e-8 && (after.1.1 - own.1.1).abs() < 1e-8);
        let character = test_world.with(system, move |s| s.with_combat_fit(ship, |fit, _, _| fit.character_inputs().trained.into_iter().map(|row| (row.type_id, row.level)).collect::<Vec<_>>())).await.flatten().unwrap();
        assert!(!character.is_empty());
        assert_eq!(live.read(|fit| fit.character_inputs().trained.into_iter().map(|row| (row.type_id, row.level)).collect::<Vec<_>>()).await, character);
        let ball = test_world.with(system, move |s| s.pilot_ball(space).cloned()).await.unwrap().unwrap();
        assert_eq!(ball.pos, pos);
        assert_eq!(ball.cloak, CLOAK_RESTORE);
        assert_eq!(prepared.cloak_until, Some(ball.cloak_until));
        assert!(ball.cloak_until >= start + 60 * infinity_core::consts::FILETIME_SECOND);
        test_world.remove_ball(system, ship).await.expect("first selected hull");
        crate::services::combat::forget_ship(ship);
        let second = items::creation::positioned(&test_db, TYPE_CAPSULE, space, system, pos).await.unwrap();
        characters::selection::set_ship(&test_db, space, second).await.unwrap();
        let selected = characters::selection::selected(&test_db, space, account).await.unwrap().unwrap();
        let prepared = super::prepare(&test_db, &test_world, &sde, space, &selected, |n| n.dispatch(&test_world, None)).await.unwrap();
        assert_eq!(prepared.ship_id, second);
        assert_eq!(live.read(move |fit|
            (fit.owner_inputs().unwrap().ship, fit.source_type(Key::Ship), fit.source_type(Key::Module(module)), fit.source_type(Key::Charge(module)))).await,
            (second, Some(TYPE_CAPSULE), None, None));
        let after = test_world.with(live.system, own_state).await.unwrap();
        assert_eq!(after.0, own.0);
        assert_eq!(after.2, own.2);
        assert!((after.1.0 - own.1.0).abs() < 1e-8 && (after.1.1 - own.1.1).abs() < 1e-8);
        assert_eq!(live.read(|fit| fit.character_inputs().trained.into_iter().map(|row| (row.type_id, row.level)).collect::<Vec<_>>()).await, character);
        for character in [docked, space] {
            assert!(test_db.query("SELECT 1 FROM game.map_visits WHERE character_id=$1", &[&character]).await.unwrap().is_empty());
        }
        let after = test_db.query("SELECT source, source_id FROM game.map_events WHERE solar_system_id=$1 ORDER BY source, source_id", &[&system]).await.unwrap()
            .iter().map(|r| (r.get::<_, i16>("source"), r.get::<_, i64>("source_id"))).collect::<Vec<_>>();
        assert_eq!(after, before);
    }).await;
    if let Some(live) = recipient {
        live.close().await;
    }
    for character in [docked, space] {
        let owned = db.query("SELECT item_id FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
        for row in &owned {
            let id: i64 = row.get("item_id");
            world.remove_ball(system, id).await;
            crate::services::combat::forget_ship(id);
            db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
        }
        world.flush_positions().await.unwrap();
        db.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&character]).await.unwrap();
        db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
        for row in &owned {
            let id: i64 = row.get("item_id");
            db.execute("DELETE FROM game.items WHERE location_id=$1 AND owner_id=$2", &[&id, &character]).await.unwrap();
        }
        db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.map_visits WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    }
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn absent_identity_leaves_zero_registry_untouched_and_calls_observer() {
    use infinity_core::session::OnlineSession;
    use crate::{config::deployment::DatabaseConfig, db::Db, sde::Sde, services::identity::Identity, space::World};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let cfg = crate::config::Config::default();
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(0, notify_tx);
    world.set_online_session(0, Some(OnlineSession { character_id: 0, user_id: None, session_id: 0, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    let identity = Identity { char_id: 0, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback_called = called.clone();
    super::leave(&identity, None, &db, &world, &chat, &sde, None, None, move || callback_called.store(true, std::sync::atomic::Ordering::Relaxed)).await.unwrap();
    assert!(called.load(std::sync::atomic::Ordering::Relaxed));
    assert!(world.is_online(0));
    assert!(world.online_session(0).is_some());
    world.notify_pilot(0, "test", vec![]);
    assert_eq!(notify_rx.try_recv().unwrap().0, "test");
    world.unregister_pilot_generation(0, None);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn ship_only_session_change_refreshes_world_without_presence_notifications() {
    use infinity_core::session::OnlineSession;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db, 1).await;
    let world = client.world.clone();
    let _chat = client.chat.clone();
    let (client_tx, mut client_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = client_tx.clone();
    world.register_pilot(1, client_tx);
    client.change_session(vec![
        ("charid", Value::Int(1)), ("shipid", Value::Int(100)),
        ("stationid", Value::Int(10)), ("solarsystemid2", Value::Int(20)),
        ("corpid", Value::Int(2)), ("allianceid", Value::Int(3)),
        ("warfactionid", Value::Int(4)), ("fleetid", Value::Int(5)),
        ("role", Value::Int(6)), ("corprole", Value::Int(7)),
    ]);
    let (peer_tx, mut peer_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(2, peer_tx);
    world.set_online_session(2, Some(OnlineSession {
        character_id: 2, user_id: None, session_id: 2, ship_id: Some(200),
        station_id: Some(10), system_id: Some(20), corporation_id: Some(2),
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    client.change_session(vec![("shipid", Value::Int(101))]);
    let snapshot = world.online_session(1).unwrap();
    assert_eq!(snapshot.character_id, 1);
    assert_eq!(snapshot.ship_id, Some(101));
    assert_eq!(snapshot.station_id, Some(10));
    assert_eq!(snapshot.system_id, Some(20));
    assert_eq!(snapshot.corporation_id, Some(2));
    assert_eq!(snapshot.alliance_id, Some(3));
    assert_eq!(snapshot.faction_id, Some(4));
    assert_eq!(snapshot.fleet_id, Some(5));
    assert_eq!(snapshot.role, 6);
    assert_eq!(snapshot.corporation_role, 7);
    assert!(peer_rx.try_recv().is_err());
    assert!(client_rx.try_recv().is_err());
    world.notify_pilot(1, "original-channel", vec![]);
    assert_eq!(client_rx.try_recv().unwrap().0, "original-channel");
    world.unregister_pilot_generation(2, None);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn stale_presence_generation_cannot_revoke_replacement_snapshot() {
    use infinity_core::session::OnlineSession;
    use crate::services::identity::Identity;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db, 1).await;
    let world = client.world.clone();
    let chat = client.chat.clone();
    let (old_tx, mut old_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = old_tx;
    client.change_session(vec![
        ("charid", Value::Int(1)), ("shipid", Value::Int(100)),
        ("stationid", Value::Int(10)), ("solarsystemid2", Value::Int(20)),
    ]);
    let old_identity = Identity::of_client(&client);
    let old_sid = client.session.sid;
    let (peer_tx, mut peer_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(2, peer_tx);
    world.set_online_session(2, Some(OnlineSession {
        character_id: 2, user_id: None, session_id: 2, ship_id: Some(200),
        station_id: Some(10), system_id: Some(20), corporation_id: None,
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    super::online(&old_identity, &world, &chat, replacement_tx, None, OnlineSession {
        character_id: 1, user_id: None, session_id: old_sid + 1, ship_id: Some(101),
        station_id: Some(10), system_id: Some(20), corporation_id: None,
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    });
    assert_eq!(peer_rx.try_recv().unwrap().0, "OnCharNowInStation");
    assert!(peer_rx.try_recv().is_err());
    super::offline_generation(&old_identity, &world, &chat, Some(old_sid));
    let replacement = world.online_session(1).unwrap();
    assert_eq!(replacement.character_id, 1);
    assert_eq!(replacement.session_id, old_sid + 1);
    assert_eq!(replacement.ship_id, Some(101));
    assert_eq!(replacement.station_id, Some(10));
    assert!(old_rx.try_recv().is_err());
    assert!(peer_rx.try_recv().is_err());
    world.unregister_pilot_generation(2, None);
}
