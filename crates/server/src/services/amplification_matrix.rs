//! Closed amplification-matrix compatibility for the HUD controller.
//! `amplificationMatrix/client/controller.py` consumes the packed notification.
//! No data source is accessed; matrix skills and deposits are not modelled.

use super::{notify_error, Call, CallResult};
use infinity_core::messages;
use crate::client::Client;
use marshal::Value;

pub(super) fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "amplificationMatrixSolarsystemMgr" { return None; }
    match call.method {
        "FetchMatrixPacked" => {
            c.notify("OnMatrixPacked", vec![Value::dict([
                ("slot1MatrixID", Value::Int(0)),
                ("slot1MatrixTypeID", Value::Int(0)),
                ("slot1DurationEndTime", Value::Int(0)),
                ("slot1CdEndTime", Value::Int(0)),
                ("slot2MatrixID", Value::Int(0)),
                ("slot2MatrixTypeID", Value::Int(0)),
                ("slot2DurationEndTime", Value::Int(0)),
                ("slot2CdEndTime", Value::Int(0)),
            ])]);
            Some(Ok(Value::None))
        }
        "ActiveSkill" | "InactiveSkill" | "SyncMatrixDbuff" | "TransferCash" =>
            Some(Err(notify_error(messages::AMPLIFICATION_MATRIX_UNAVAILABLE))),
        _ => None,
    }
}
