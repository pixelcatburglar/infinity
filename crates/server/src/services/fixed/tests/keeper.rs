use super::*;

#[test]
fn keeper_fixed_contract_and_ownership() {
    let kwargs = Value::Dict(vec![]);
    for (method, expected) in [("GetOperationSpawnpointSolarSystem", Value::None), ("IsOperationSite", Value::Bool(false))] {
        let call = Call { service: "keeper", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let value = root(&call).unwrap().unwrap();
        assert_eq!(value, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
        assert!(root(&Call { service: "other", ..call }).is_none());
        assert!(station(&call).is_none());
    }
    let editor = Call { service: "keeper", method: "GetLevelEditor", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let super::super::super::CallError::User { msg, dict } = root(&editor).unwrap().unwrap_err() else { panic!("expected editor refusal") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::npe::LEVEL_EDITOR_UNAVAILABLE))]);
    let wire = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
    assert!(root(&Call { service: "other", ..editor }).is_none());
    for method in ["Bind", "unknown", "GetCurrentDungeonForCharacter"] {
        assert!(root(&Call { method, ..editor }).is_none());
    }
    let dungeon = Call { method: "GetCurrentDungeonForCharacter", ..editor };
    assert_eq!(station(&dungeon).unwrap().unwrap(), Value::None);
    assert!(root(&Call { service: "beyonce", method: "GetOperationSpawnpointPosition", ..editor }).is_none());
}
