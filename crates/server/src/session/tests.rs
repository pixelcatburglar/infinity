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
fn chat_session_station_filter_and_equality() {
    let mut session = Session::new(1);
    session.apply(vec![("charid", Value::Int(1))]);
    let absent = ChatSession::from_session(&session).unwrap();
    for value in [Value::Int(0), Value::Int(-1), Value::str("station"), Value::None] {
        session.apply(vec![("stationid", value)]);
        assert_eq!(ChatSession::from_session(&session).unwrap().station_id, None);
    }
    session.apply(vec![("stationid", Value::Int(42))]);
    let docked = ChatSession::from_session(&session).unwrap();
    assert_eq!(docked.station_id, Some(42));
    assert_ne!(docked, absent);
}
