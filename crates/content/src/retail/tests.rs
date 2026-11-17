use super::*;

#[test]
fn retail_captures_decode() {
    for (name, _) in CALLS {
        assert!(calls().contains_key(name), "{name}");
    }
    let prices = average_prices();
    assert!(prices.len() > 10_000);
    let plex = prices.iter().find(|p| p.0 == 44992).unwrap();
    assert!(plex.1 > 1_000_000.0);
    assert_eq!(&cdn("achievements").unwrap().data[4..8], b"ACHV");
    assert!(cached_call("bannedwords", "fetch_regexes", &[Value::str("blocked")]).is_some());
}
