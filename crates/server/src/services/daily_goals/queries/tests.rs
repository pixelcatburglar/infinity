use super::*;

#[test]
fn history_token_rejects_wrong_owner_version_and_cursor_order() {
    let good = Token { owner: 7, version: 1, high_time: 20, high_id: "00000000-0000-0000-0000-000000000002".into(), last_time: 10, last_id: "00000000-0000-0000-0000-000000000001".into() };
    let text = serde_json::to_string(&good).unwrap();
    assert!(Token::parse("", 7).unwrap().is_none());
    assert!(Token::parse(&text, 7).unwrap().is_some());
    assert!(matches!(Token::parse(&text, 8), Err(Error::Invalid)));
    assert!(matches!(Token::parse("not-json", 7), Err(Error::Invalid)));
    assert!(matches!(Token::parse(&text.replace("000000000002", "00000000000A"), 7), Err(Error::Invalid)));
    assert!(matches!(Token::parse(&text.replace("\"owner\":7", "\"owner\":7,\"unknown\":1"), 7), Err(Error::Invalid)));
    let wrong_version = Token { version: 2, ..good };
    assert!(matches!(Token::parse(&serde_json::to_string(&wrong_version).unwrap(), 7), Err(Error::Invalid)));
    let inverted = Token { version: 1, last_time: 21, ..wrong_version };
    assert!(matches!(Token::parse(&serde_json::to_string(&inverted).unwrap(), 7), Err(Error::Invalid)));
}
