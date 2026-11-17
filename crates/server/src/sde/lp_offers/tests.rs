use super::*;

fn row(corporation_id: i64, offer_id: i64, required_items: &str) -> crate::dao::sde::lp_offers::OfferRow {
    crate::dao::sde::lp_offers::OfferRow { corporation_id, offer_id, type_id: 1, quantity: 1, lp_cost: 0, isk_cost: 0, ak_cost: 0, required_items: required_items.into() }
}

#[test]
fn requirements_preserve_order_and_reject_invalid_data() {
    let offers = LpOffers::from_rows(vec![row(1, 1, r#"[{"type_id":3,"quantity":2},{"type_id":1,"quantity":4},{"type_id":3,"quantity":5}]"#)]).unwrap();
    assert_eq!(offers.get(1, 1).unwrap().required_items.iter().map(|x| (x.type_id, x.quantity)).collect::<Vec<_>>(), vec![(3, 2), (1, 4), (3, 5)]);
    for invalid in [r#"{}"#, r#"[{"type_id":1}]"#, r#"[{"type_id":0,"quantity":1}]"#, r#"[{"type_id":1,"quantity":-1}]"#, r#"[{"type_id":2147483648,"quantity":1}]"#] {
        let Err(error) = LpOffers::from_rows(vec![row(7, 8, invalid)]) else { panic!("accepted {invalid}") };
        let message = format!("{error:#}");
        assert!(message.contains("corporation_id=7") && message.contains("offer_id=8"), "invalid {invalid}: {message}");
    }
}

#[test]
fn empty_and_missing_corporations_have_no_offers() {
    assert!(LpOffers::from_rows(Vec::new()).unwrap().is_empty());
    let offers = LpOffers::from_rows(vec![row(1, 1, "[]")]).unwrap();
    assert_eq!(offers.list(1).len(), 1);
    assert!(offers.list(9).is_empty() && offers.get(1, 2).is_none() && offers.get(9, 1).is_none());
}
