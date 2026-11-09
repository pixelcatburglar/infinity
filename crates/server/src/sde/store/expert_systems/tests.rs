use super::*;

#[test]
fn embedded_policy_and_invalid_entries() {
    let content = include_str!("../../../../content/expert_system_store.json");
    let offers = parse(content, |_| true, |_| true).unwrap();
    assert_eq!(offers, vec![ExpertOffer { type_id: 57207, quantity: 1, plex_price: 15 }, ExpertOffer { type_id: 86227, quantity: 1, plex_price: 50 }]);
    let value: serde_json::Value = serde_json::from_str(content).unwrap();
    let mut invalid = Vec::new();
    let mut v = value.clone(); v["version"] = 2.into(); invalid.push(v);
    let mut v = value.clone(); v["extra"] = true.into(); invalid.push(v);
    let mut v = value.clone(); v["offers"][0]["extra"] = true.into(); invalid.push(v);
    for key in ["type_id", "quantity", "plex_price"] {
        for n in [0, -1] { let mut v = value.clone(); v["offers"][0][key] = n.into(); invalid.push(v); }
    }
    for key in ["source_url", "observed_on", "price_basis"] { let mut v = value.clone(); v["offers"][0][key] = "".into(); invalid.push(v); }
    let mut v = value.clone(); let duplicate = v["offers"][0].clone(); v["offers"].as_array_mut().unwrap().push(duplicate); invalid.push(v);
    for v in invalid { assert!(parse(&v.to_string(), |_| true, |_| true).is_err()); }
    assert!(parse("{", |_| true, |_| true).is_err());
    assert!(parse(content, |_| false, |_| true).is_err());
    assert!(parse(content, |_| true, |_| false).is_err());
}
