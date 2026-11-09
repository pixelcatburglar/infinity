use super::*;
use crate::gateway::message::message;

mod database;

#[test]
fn milestone_wire_round_trips_both_targets() {
    let response = message("eve_public.character.skill.plan.milestone.GetAllResponse").unwrap();
    for (kind, level) in [("type", None), ("skill", Some(3))] {
        let row = dao::Milestone { milestone_id: vec![1; UUID_LEN], plan_id: vec![2; UUID_LEN], kind: kind.into(), type_id: 17, level, description: "说明".into() };
        let entry = encode(&row, &response).unwrap();
        let data = msg_field(&entry, "data").unwrap();
        assert_eq!(uuid(&data, "skill_plan"), Some(vec![2; UUID_LEN]));
        assert_eq!(uuid(&entry, "identifier"), Some(vec![1; UUID_LEN]));
        assert_eq!(data.has_field_by_name("train_to_type"), level.is_none());
        assert_eq!(data.has_field_by_name("skill"), level.is_some());
        if let Some(level) = level {
            let skill = msg_field(&data, "skill").unwrap();
            assert_eq!(numeric_id(&skill, "skill_type"), Some(17));
            assert_eq!(skill.get_field_by_name("level").unwrap().as_u32(), Some(level as u32));
        } else { assert_eq!(numeric_id(&data, "train_to_type"), Some(17)); }
    }
}

#[test]
fn identifiers_reject_absent_zero_and_short() {
    let desc = message("eve_public.character.skill.plan.milestone.GetAllRequest").unwrap();
    let mut request = DynamicMessage::new(desc);
    assert!(uuid(&request, "skill_plan").is_none());
    let mut id = child(&request.descriptor(), "skill_plan").unwrap();
    for bytes in [vec![0; UUID_LEN], vec![1; UUID_LEN - 1]] {
        set(&mut id, "uuid", PV::Bytes(Bytes::from(bytes))).unwrap();
        set(&mut request, "skill_plan", PV::Message(id.clone())).unwrap();
        assert!(uuid(&request, "skill_plan").is_none());
    }
}
