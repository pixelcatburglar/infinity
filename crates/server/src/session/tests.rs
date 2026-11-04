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
