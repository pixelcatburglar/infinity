use super::valid_security;
use crate::services::slash::tests::support;
use infinity_core::consts::{CATEGORY_SHIP, CLIENT_MAX_SHORT_AMOUNT, FLAG_CARGO, FLAG_HANGAR, GM_ROLES};
use infinity_core::messages;
use infinity_core::session::OnlineSession;
use marshal::Value;

#[test]
fn security_requires_finite_inclusive_range() {
    for value in [-10.0, 10.0] { assert!(valid_security(value)); }
    for value in [-10.01, 10.01, f64::NAN, f64::INFINITY] { assert!(!valid_security(value)); }
}

fn text(result: crate::services::CallResult) -> String {
    match result.unwrap() { Value::Unicode(text) => text, other => panic!("expected unicode, got {other:?}") }
}

fn text_ref(result: &crate::services::CallResult) -> String {
    match result { Ok(Value::Unicode(text)) => text.clone(), other => panic!("expected unicode, got {other:?}") }
}
fn reason(error: crate::services::CallResult) -> String {
    match error.unwrap_err() {
        crate::services::CallError::User { dict, .. } => match &dict[0].1 { Value::Unicode(text) => text.clone(), other => panic!("expected unicode reason, got {other:?}") },
        other => panic!("expected user error, got {other:?}"),
    }
}

fn snapshot(character: i64, session: i64, station: Option<i64>, system: Option<i64>) -> OnlineSession {
    OnlineSession {
        character_id: character, user_id: Some(1), session_id: session, ship_id: None,
        station_id: station, system_id: system, corporation_id: None, alliance_id: None,
        faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }
}

async fn docked_client(db: &crate::db::Db, account: i64, character: i64) -> (crate::client::Client, i64, i64) {
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character).await;
    client.login.user_id = account;
    client.login.role = GM_ROLES;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).expect("SDE station");
    let (station_id, system_id) = (station.station_id, station.solar_system_id.unwrap());
    client.change_session(vec![("userid", Value::Int(account)), ("charid", Value::Int(character)),
        ("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system_id))]);
    client.outbox.clear();
    (client, station_id, system_id)
}

async fn insert_ship(db: &crate::db::Db, type_id: i32, owner: i64, location: i64, flag: i32, quantity: i64, singleton: bool, name: Option<&str>) -> i64 {
    db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, name) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING item_id",
        &[&type_id, &owner, &location, &flag, &quantity, &singleton, &name]).await.unwrap().get("item_id")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn readonly_location_and_online_snapshot_are_routed() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let mut ids: Vec<i64> = Vec::new();
    let (mut client, station_id, system_id) = docked_client(&test_db, fixture.account, fixture.character).await;
    let character = fixture.character;
    let result = crate::tests::catch_scenario(async {
        let system_name = client.sde.map.system(system_id).and_then(|s| s.name.clone());
        client.session.apply(vec![("solarsystemid", Value::None)]);
        let location = text(support::rpc(&mut client, "/where").await);
        assert_eq!(location, messages::gm::gm_where(Some(system_id), system_name.as_deref(), Some(station_id)));
        // In space only the station disappears; the stale solarsystemid is not consulted.
        client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system_id))]);
        let space = text(support::rpc(&mut client, "/where").await);
        assert_eq!(space, messages::gm::gm_where(Some(system_id), system_name.as_deref(), None));
        client.session.apply(vec![("solarsystemid2", Value::None)]);
        assert_eq!(text(support::rpc(&mut client, "/where").await), messages::gm::gm_where(None, None, None));
        client.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid", Value::None), ("solarsystemid2", Value::Int(system_id))]);
        for (line, usage) in [("/where x", messages::gm::GM_WHERE_USAGE), ("/who x", messages::gm::GM_WHO_USAGE),
            ("/wallet x", messages::gm::GM_WALLET_USAGE), ("/hangar x", messages::gm::GM_HANGAR_USAGE)] {
            assert_eq!(reason(support::rpc(&mut client, line).await), usage, "{line}");
        }
        client.login.role = 0;
        assert_eq!(reason(support::rpc(&mut client, "/who").await), messages::gm::GM_ROLE_REQUIRED);
        client.login.role = GM_ROLES;
        // The live snapshot wins over the persisted position; a replaced
        // session contributes only its newest snapshot.
        test_db.execute("UPDATE game.characters SET solar_system_id=1, station_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
        let _stale = support::register_online(&client.world, snapshot(character, 1, Some(1), Some(1)));
        let _live = support::register_online(&client.world, snapshot(character, 2, Some(station_id), Some(system_id)));
        let character_name: Option<String> = test_db.query_one("SELECT name FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap().get("name");
        let who = text(support::rpc(&mut client, "/who").await);
        let current_row = messages::gm::gm_who_row(character, character_name.as_deref(), Some(system_id), system_name.as_deref(), Some(station_id));
        let stale_name = client.sde.map.system(1).and_then(|s| s.name.as_deref());
        let stale_row = messages::gm::gm_who_row(character, character_name.as_deref(), Some(1), stale_name, Some(1));
        assert_eq!(who.lines().collect::<Vec<_>>(), vec![messages::gm::gm_who_header(1).as_str(), current_row.as_str()]);
        assert!(!who.lines().any(|line| line == stale_row));
        ids = test_db.query(
            "INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, solar_system_id, station_id)
             SELECT $1, 'atomicity-extra-' || i, 1, 1, 1, 1, 1, NULL FROM generate_series(1::bigint, $2::bigint) AS i RETURNING character_id",
            &[&fixture.account, &(super::super::LIST_ROW_LIMIT as i64)])
            .await.unwrap().iter().map(|row| row.get("character_id")).collect();
        let mut receivers = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            receivers.push(support::register_online(&client.world, snapshot(*id, 3 + index as i64, Some(station_id), Some(system_id))));
        }
        let who_result = support::rpc(&mut client, "/who").await;
        let who = text_ref(&who_result);
        let mut sorted = ids.clone();
        sorted.push(character);
        sorted.sort_unstable();
        let names = test_db.query("SELECT character_id, name FROM game.characters WHERE character_id = ANY($1)", &[&sorted]).await.unwrap()
            .iter().map(|row| (row.get::<_, i64>("character_id"), row.get::<_, Option<String>>("name")))
            .collect::<std::collections::HashMap<_, _>>();
        let mut expected = vec![messages::gm::gm_who_header(sorted.len() as i64)];
        expected.extend(sorted.iter().take(super::super::LIST_ROW_LIMIT).map(|id|
            messages::gm::gm_who_row(*id, names.get(id).unwrap().as_deref(), Some(system_id), system_name.as_deref(), Some(station_id))));
        expected.push(messages::gm::gm_rows_truncated(1));
        assert_eq!(who.lines().collect::<Vec<_>>(), expected.iter().map(String::as_str).collect::<Vec<_>>());
        for receiver in &mut receivers {
            assert!(receiver.try_recv().is_err(), "read-only command notified a pilot");
        }
        // The wire encodes the same unicode text the RPC returned.
        assert_eq!(marshal::loads(&support::wire(who_result)).unwrap(), Value::Unicode(who));
    }).await;
    db.execute("DELETE FROM game.characters WHERE character_id = ANY($1)", &[&ids]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn readonly_wallet_and_hangar_preserve_exact_scoped_values() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let (mut client, station_id, _) = docked_client(&test_db, fixture.account, fixture.character).await;
    let (character, account) = (fixture.character, fixture.account);
    let result = crate::tests::catch_scenario(async {
        let ship_type = client.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("SDE ship").type_id;
        let named_ship_type = client.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.type_id != ship_type && t.name.is_some()).expect("second SDE ship").type_id;
        let non_ship_type = client.sde.types.iter().find(|t| t.published == Some(true) && t.category_id.is_none_or(|category| category != CATEGORY_SHIP)).expect("non-ship SDE type").type_id;
        let max_isk = "9999999999999999999999999999999999.99";
        let max_plex = "999999999999999999999999999999999999";
        test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&character, &max_isk]).await.unwrap();
        test_db.execute("UPDATE game.accounts SET plex_balance=$2::text::numeric WHERE user_id=$1", &[&account, &max_plex]).await.unwrap();
        // The session userid deliberately differs from the authenticated account.
        client.session.apply(vec![("userid", Value::Int(other.account))]);
        let wallet = text(support::rpc(&mut client, "/wallet").await);
        let expected = messages::gm::gm_wallet(max_isk, max_plex, &crate::isk::personal_client_cap().decimal(), CLIENT_MAX_SHORT_AMOUNT);
        assert_eq!(wallet, expected, "{wallet}");
        // Balances and the session are unchanged after the read.
        let isk_after: String = test_db.query_one("SELECT balance::text AS value FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap().get("value");
        let plex_after: String = test_db.query_one("SELECT plex_balance::text AS value FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap().get("value");
        assert_eq!((isk_after, plex_after), (max_isk.to_string(), max_plex.to_string()));
        assert_eq!(client.session.int("stationid"), Some(station_id));
        assert!(client.outbox.is_empty(), "read-only command queued packets");
        // Hangar scope: the active ship is a qualifying row shown first once;
        // packed quantity sums, other owners/locations/cargo/non-ships are absent.
        let packed = insert_ship(&test_db, ship_type, character, station_id, FLAG_HANGAR, 3, false, None).await;
        let spare = insert_ship(&test_db, named_ship_type, character, station_id, FLAG_HANGAR, 1, true, Some("")).await;
        let current = insert_ship(&test_db, ship_type, character, station_id, FLAG_HANGAR, 1, true, Some("Flagship")).await;
        for (owner, location, flag, type_id, singleton) in [
            (other.character, station_id, FLAG_HANGAR, ship_type, true),
            (character, station_id + 1, FLAG_HANGAR, ship_type, true),
            (character, packed, FLAG_CARGO, ship_type, true),
            (character, station_id, FLAG_HANGAR, non_ship_type, true),
        ] { insert_ship(&test_db, type_id, owner, location, flag, 1, singleton, None).await; }
        let before: i64 = test_db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap().get("count");
        client.change_session(vec![("shipid", Value::Int(current))]);
        client.outbox.clear();
        let hangar = text(support::rpc(&mut client, "/hangar").await);
        let ship_name = client.sde.types.get(ship_type).and_then(|t| t.name.clone());
        let named_type = client.sde.types.get(named_ship_type).and_then(|t| t.name.clone()).unwrap();
        let packed_row = messages::gm::gm_hangar_row(packed, ship_type, ship_name.as_deref(), 3, false, false);
        let current_row = messages::gm::gm_hangar_row(current, ship_type, Some("Flagship"), 1, true, true);
        let mut others = vec![(packed, packed_row.clone()), (spare, messages::gm::gm_hangar_row(spare, named_ship_type, Some(&named_type), 1, true, false))];
        others.sort_by_key(|(id, _)| *id);
        let mut expected = vec![messages::gm::gm_hangar_header(station_id, 3, 5), current_row.clone()];
        expected.extend(others.into_iter().map(|(_, row)| row));
        let lines: Vec<_> = hangar.lines().collect();
        assert_eq!(lines, expected.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(lines.iter().filter(|line| **line == current_row).count(), 1);
        assert!(lines.contains(&packed_row.as_str()));
        // Fill past the row limit, then make the last inserted row the active
        // ship: it is beyond the 50-row window in DAO order and only the
        // current-first rule and its name batch put it at the top.
        let extra = super::super::LIST_ROW_LIMIT - 2;
        let mut filling = Vec::new();
        for _ in 0..extra { filling.push(insert_ship(&test_db, ship_type, character, station_id, FLAG_HANGAR, 1, true, None).await); }
        let paged_current = *filling.last().unwrap();
        test_db.execute("UPDATE game.items SET name='Paged flagship' WHERE item_id=$1", &[&paged_current]).await.unwrap();
        client.change_session(vec![("shipid", Value::Int(paged_current))]);
        client.outbox.clear();
        let hangar = text(support::rpc(&mut client, "/hangar").await);
        let total = 3 + extra;
        let paged_row = messages::gm::gm_hangar_row(paged_current, ship_type, Some("Paged flagship"), 1, true, true);
        let mut others = vec![
            (packed, packed_row),
            (spare, messages::gm::gm_hangar_row(spare, named_ship_type, Some(&named_type), 1, true, false)),
            (current, messages::gm::gm_hangar_row(current, ship_type, Some("Flagship"), 1, true, false)),
        ];
        others.extend(filling.iter().filter(|id| **id != paged_current).map(|id|
            (*id, messages::gm::gm_hangar_row(*id, ship_type, ship_name.as_deref(), 1, true, false))));
        others.sort_by_key(|(id, _)| *id);
        let pushed_out = others.last().unwrap().1.clone();
        let mut expected = vec![messages::gm::gm_hangar_header(station_id, total as i64, (total + 2) as i128), paged_row.clone()];
        expected.extend(others.into_iter().take(super::super::LIST_ROW_LIMIT - 1).map(|(_, row)| row));
        expected.push(messages::gm::gm_rows_truncated(1));
        let lines: Vec<_> = hangar.lines().collect();
        assert_eq!(lines, expected.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(lines.len() - 2, super::super::LIST_ROW_LIMIT);
        assert_eq!(lines[1], paged_row);
        assert_eq!(lines.iter().filter(|line| **line == paged_row).count(), 1);
        assert!(!lines.contains(&pushed_out.as_str()));
        let after: i64 = test_db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap().get("count");
        assert_eq!(after, before + extra as i64, "reads changed inventory");
        // Undocking removes the station scope and the command refuses.
        client.change_session(vec![("stationid", Value::None), ("shipid", Value::None)]);
        client.outbox.clear();
        assert_eq!(reason(support::rpc(&mut client, "/hangar").await), messages::gm::GM_SHIP_DOCKED_REQUIRED);
        assert!(client.outbox.is_empty(), "read-only command queued packets");
    }).await;
    db.execute("DELETE FROM game.items WHERE owner_id = ANY($1)", &[&vec![character, other.character]]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}
