use super::*;

#[test]
fn exact_descriptor_and_text_roundtrip() {
    let (request, response) = descriptors("eve_public.dailygoal.api.GetAllCurrentRequest").unwrap();
    let input = DynamicMessage::new(request.clone());
    let text = encode(&input, &request).unwrap();
    assert_eq!(DynamicMessage::parse_text_format(request, &text).unwrap(), input);
    assert!(encode(&input, &response).is_err());
    assert!(descriptors("eve_public.dailygoal.api.RedeemAllRequest").is_err());
    assert!(descriptors("eve_public.dailygoal.api.*").is_err());
}
