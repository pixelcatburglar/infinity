use super::*;
use crate::services::CallError;

#[test]
fn qa_ownership_and_results_are_exact() {
    assert_eq!(qa_dispatch("node_graph", "qa_subscribe").unwrap().unwrap(), Value::Dict(vec![]));
    assert_eq!(qa_dispatch("node_graph", "qa_unsubscribe").unwrap().unwrap(), Value::None);
    for method in ["qa_start_node_graph", "qa_stop_node_graph", "qa_start_node", "qa_stop_node", "qa_stop_active_nodes"] {
        match qa_dispatch("node_graph", method) {
            Some(Err(CallError::User { msg, dict })) => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::npe::NODE_GRAPH_UNAVAILABLE))]);
            }
            other => panic!("unexpected result for {method}: {other:?}"),
        }
        assert!(qa_dispatch("other", method).is_none());
    }
    for method in ["qa_subscribe", "qa_unsubscribe"] {
        assert!(qa_dispatch("other", method).is_none());
    }
    for method in ["qa_unknown", "qa_", "unrelated", "process_client_graph_message"] {
        assert!(qa_dispatch("node_graph", method).is_none());
    }
}
