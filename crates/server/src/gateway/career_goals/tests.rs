use super::*;

#[test]
fn goals_build() {
    assert_eq!(career_goal_definitions().unwrap().get_field_by_name("goals").unwrap().as_list().unwrap().len(), 4);
    career_goal_states().unwrap();
}
