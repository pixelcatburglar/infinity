//! Relay messages mirror nodegraph.client.graph.
//! QA calls mirror nodegraph.client.service and log_graph.
//! No database is used; server graph execution and QA controls are unsupported.
//! Subscriptions are empty and graph relay messages have no running recipient.

use super::super::{notify_error, Call, CallResult};
use crate::client::Client;
use marshal::Value;
use tracing::debug;

pub(super) fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        // Client graphs talk to the server graph that started them; none run here, so
        // there is no server graph instance to deliver to (nodegraph.client.graph).
        ("node_graph", "process_client_graph_message") | ("node_graph", "process_client_blackboard_request") => {
            debug!(user = c.login.user_id, "node_graph::{} with no server graph: {:?}", call.method, call.args);
            Ok(Value::None)
        }
        _ => return qa_dispatch(call.service, call.method),
    })
}

fn qa_dispatch(service: &str, method: &str) -> Option<CallResult> {
    Some(match (service, method) {
        ("node_graph", "qa_subscribe") => Ok(Value::Dict(vec![])),
        ("node_graph", "qa_unsubscribe") => Ok(Value::None),
        ("node_graph", "qa_start_node_graph" | "qa_stop_node_graph" | "qa_start_node" | "qa_stop_node" | "qa_stop_active_nodes") =>
            Err(notify_error(infinity_core::messages::npe::NODE_GRAPH_UNAVAILABLE)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
