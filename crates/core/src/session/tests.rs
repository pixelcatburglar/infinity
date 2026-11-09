use super::*;

#[test]
fn attributes_are_read_only_and_sorted() {
    let mut session = Session::new(7);
    session.apply(vec![("z", Value::Int(2)), ("a", Value::Int(1))]);
    let rows: Vec<_> = session.attributes().collect();
    assert_eq!(rows, vec![("a", &Value::Int(1)), ("z", &Value::Int(2))]);
    assert_eq!(session.int("a"), Some(1));
}

#[test]
fn online_session_station_filter_and_equality() {
    let mut session = Session::new(1);
    session.apply(vec![("charid", Value::Int(1))]);
    let absent = OnlineSession::from_session(&session).unwrap();
    for value in [Value::Int(0), Value::Int(-1), Value::str("station"), Value::None] {
        session.apply(vec![("stationid", value)]);
        assert_eq!(OnlineSession::from_session(&session).unwrap().station_id, None);
    }
    session.apply(vec![("stationid", Value::Int(42))]);
    let docked = OnlineSession::from_session(&session).unwrap();
    assert_eq!(docked.station_id, Some(42));
    assert_ne!(docked, absent);
}

#[test]
fn online_session_generation_comes_from_session_sid() {
    let mut session = Session::new(77);
    session.apply(vec![("charid", Value::Int(4)), ("shipid", Value::Int(9)), ("userid", Value::Int(3))]);
    let online = OnlineSession::from_session(&session).unwrap();
    assert_eq!((online.session_id, online.user_id, online.ship_id), (77, Some(3), Some(9)));
}

#[test]
fn session_pairs_skip_malformed_entries_without_reordering() {
    let values = vec![
        Value::tuple([Value::str("corpid"), Value::Int(10)]),
        Value::list([Value::unicode("role"), Value::Int(2)]),
        Value::tuple([Value::Int(1), Value::Int(3)]),
        Value::list([Value::str("short")]),
        Value::None,
        Value::tuple([Value::str("corpid"), Value::None]),
    ];
    assert_eq!(decode_pairs(&values), vec![("corpid".into(), Value::Int(10)), ("role".into(), Value::Int(2)), ("corpid".into(), Value::None)]);
}

#[test]
fn projection_tracks_modeled_fields_and_preserves_docked_system() {
    let mut raw = Session::new(9);
    raw.apply(vec![("charid", Value::Int(10)), ("userid", Value::Int(11)), ("solarsystemid2", Value::Int(20))]);
    let mut session = OnlineSession::from_session(&raw).unwrap();
    let pairs = [
        ("charid", Value::Int(99)), ("userid", Value::Int(12)), ("shipid", Value::Int(30)),
        ("stationid", Value::Int(40)), ("solarsystemid", Value::None), ("corpid", Value::Int(50)),
        ("allianceid", Value::Int(60)), ("warfactionid", Value::Int(70)), ("fleetid", Value::Int(80)),
        ("role", Value::Int(90)), ("corprole", Value::Int(100)),
        ("rolesAtAll", Value::Int(101)), ("rolesAtHQ", Value::Int(102)), ("rolesAtBase", Value::Int(103)),
        ("rolesAtOther", Value::Int(104)), ("hqID", Value::Int(105)), ("corpAccountKey", Value::Int(1000)),
        ("wingid", Value::Int(5)),
    ];
    assert!(session.apply_updates(pairs.iter().map(|(key, value)| (*key, value))));
    assert_eq!((session.character_id, session.session_id, session.user_id), (10, 9, Some(12)));
    assert_eq!((session.ship_id, session.station_id, session.system_id), (Some(30), Some(40), Some(20)));
    assert_eq!((session.corporation_id, session.alliance_id, session.faction_id, session.fleet_id), (Some(50), Some(60), Some(70), Some(80)));
    assert_eq!((session.role, session.corporation_role), (90, 100));
    assert!(!session.apply_updates(pairs.iter().map(|(key, value)| (*key, value))));
    let cleared = [("userid", Value::None), ("shipid", Value::Int(0)), ("stationid", Value::Int(-1)),
        ("solarsystemid2", Value::None), ("corpid", Value::None), ("allianceid", Value::None),
        ("warfactionid", Value::None), ("fleetid", Value::None), ("role", Value::None), ("corprole", Value::str("invalid"))];
    assert!(session.apply_updates(cleared.iter().map(|(key, value)| (*key, value))));
    assert_eq!((session.user_id, session.ship_id, session.station_id, session.system_id), (None, None, None, None));
    assert_eq!((session.corporation_id, session.alliance_id, session.faction_id, session.fleet_id), (None, None, None, None));
    assert_eq!((session.role, session.corporation_role), (0, 0));
    let space = [("solarsystemid", Value::Int(123)), ("solarsystemid", Value::None)];
    assert!(session.apply_updates(space.iter().map(|(key, value)| (*key, value))));
    assert_eq!(session.system_id, Some(123));
}
