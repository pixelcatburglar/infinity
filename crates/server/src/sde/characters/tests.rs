use super::*;

fn npc(character_id: i64, corporation_id: Option<i64>, is_agent: bool, deleted: bool) -> NpcCharacter {
    NpcCharacter { character_id, corporation_id, name: None, gender: None, name_id: None, is_agent, deleted }
}

fn characters(npc_characters: Vec<NpcCharacter>) -> Characters {
    Characters::from_rows(Vec::new(), Vec::new(), npc_characters, Vec::new(), HashMap::new())
}

#[test]
fn agent_corporation_ids_keeps_sorted_unique_live_agent_corporations() {
    let characters = characters(vec![
        npc(1, Some(30), true, false),      // live agent
        npc(2, Some(10), true, false),      // live agent, sorts first
        npc(3, Some(10), true, false),      // duplicate corporation
        npc(4, Some(20), false, false),     // not an agent
        npc(5, Some(40), true, true),       // deleted agent
        npc(6, None, true, false),          // missing corporation
        npc(7, Some(0), true, false),       // nonpositive corporation
        npc(8, Some(-5), true, false),      // nonpositive corporation
    ]);
    assert_eq!(characters.agent_corporation_ids(), &[10, 30]);
    assert!(characters.npc_character(5).is_some());
    assert!(characters.npc_character(4).is_some_and(|row| !row.is_agent));
}
