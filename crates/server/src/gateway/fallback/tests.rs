use super::*;
use crate::gateway::message::{response_for, identifier, set};
use prost_reflect::Value as PV;

#[test]
fn defaults_say_absent_and_echo_ids() {
    let caller = Caller { user_id: Some(1), character_id: Some(90000001), ..Default::default() };
    let mut m = response_for("eve_public.matrix.api.GetEquippedRequest").unwrap();
    let ids = known_ids(&m, &caller);
    fill_defaults(&mut m, 4, &ids);
    assert!(format!("{m}").contains("no_equipped:true"), "{m}");

    let mut m = response_for("eve_public.pirate.corruption.api.GetSystemInfoRequest").unwrap();
    fill_defaults(&mut m, 4, &Default::default());
    assert!(format!("{m}").contains("denominator:1"), "{m}");

    let mut req = DynamicMessage::new(message("eve_public.cosmetic.ship.api.GetRequest").unwrap());
    let ship = identifier(&req.descriptor(), "ship", PV::U64(1000000000001)).unwrap();
    set(&mut req, "ship", PV::Message(ship)).unwrap();
    let mut m = response_for("eve_public.cosmetic.ship.api.GetRequest").unwrap();
    fill_defaults(&mut m, 4, &known_ids(&req, &caller));
    let s = format!("{m}");
    assert!(s.contains("1000000000001") && s.contains("90000001"), "{s}");
}
