//! Compatibility boundary for the client world_task_service remote calls.
//! Responses use fixed protobuf text and connection-local notifications, without a database.
//! World-task missions, actions and rewards are not implemented.

use super::{Call, CallResult, notify_error};
use crate::client::Client;
use marshal::Value;

/// `eveProto/generated/eve/worldtask/worldtask_pb2.py` Responsed: no missions.
const WORLD_TASK_RESPONSE: &str = "";
/// `eveProto/generated/eve/worldtask/user/user_pb2.py` Responsed: zero counts and Unix epoch.
const WORLD_TASK_USER_DATA: &str = "rest_scan_count: 0\nmax_scan_count: 0\nnext_scan_count_add_time { seconds: 0 nanos: 0 }\nunlock_fight_mission_level: 0\nunlock_support_mission_level: 0\n";

pub(super) fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("worldTaskMgr", "RequestAllTask") => {
            c.notify("OnWorldTaskResponded", vec![Value::str(WORLD_TASK_RESPONSE)]);
            Some(Ok(Value::None))
        }
        ("worldTaskMgr", "RequestUserData") => {
            c.notify("OnWorldTaskUserDataResponded", vec![Value::str(WORLD_TASK_USER_DATA)]);
            Some(Ok(Value::None))
        }
        ("worldTaskMgr", "ActiveTask" | "AbortedTask" | "Search" | "Select" | "SubmitItems" | "Complete") => {
            Some(Err(notify_error(infinity_core::messages::world_tasks::UNAVAILABLE)))
        }
        _ => None,
    }
}
