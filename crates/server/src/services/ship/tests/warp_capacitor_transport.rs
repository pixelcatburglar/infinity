//! Real framed client transport capture for ordinary warp replay.
use super::*;
use std::collections::BTreeMap;
use serde_json::{json, Value as Json};

use infinity_wire::macho::{Address, Packet};

#[path = "warp_capacitor_transport/controlled_catchup_tcp.rs"]
mod controlled_catchup_tcp;
#[path = "warp_capacitor_transport/real_service_tcp.rs"]
mod real_service_tcp;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes { write!(&mut result, "{byte:02x}").unwrap(); }
    result
}

fn object_view(value: &Value) -> Json {
    match value {
        Value::None => Json::Null,
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::Float(v) if v.is_finite() => json!(v),
        Value::Bytes(v) => json!({"t":"bytes", "hex":hex(v)}),
        Value::Unicode(v) => json!({"t":"unicode", "text":v}),
        Value::Tuple(v) => json!({"t":"tuple", "items":v.iter().map(object_view).collect::<Vec<_>>() }),
        Value::List(v) => json!({"t":"list", "items":v.iter().map(object_view).collect::<Vec<_>>() }),
        Value::Dict(v) => json!({"t":"dict", "items":v.iter().map(|(k, v)| [object_view(k), object_view(v)]).collect::<Vec<_>>() }),
        Value::Global(v) => json!({"t":"global", "name":v}),
        Value::Instance(class, state) => json!({"t":"instance", "class":object_view(class), "state":object_view(state)}),
        Value::Reduce { data, list, dict } => json!({"t":"reduce", "data":object_view(data),
            "list":list.iter().map(object_view).collect::<Vec<_>>(),
            "dict":dict.iter().map(|(k,v)| [object_view(k), object_view(v)]).collect::<Vec<_>>() }),
        Value::DbRow { descriptor, .. } => {
            let descriptor = marshal::DbRowDescriptor::from_value(descriptor).expect("DBRow descriptor");
            let columns: Vec<_> = descriptor.columns.iter().map(|(name, ty)| json!([name, *ty as i32])).collect();
            let values: Vec<_> = descriptor.columns.iter().map(|(name, _)| object_view(&descriptor.get(value, name).expect("DBRow column"))).collect();
            json!({"t":"dbrow", "columns":columns, "values":values, "virtual_columns":descriptor.virtual_columns})
        }
        other => panic!("unsupported captured object: {other:?}"),
    }
}

fn opaque(value: &Value, objects: &mut BTreeMap<String, Json>) -> Json {
    let encoded = hex(&marshal::dumps(value));
    let view = object_view(value);
    if let Some(previous) = objects.insert(encoded.clone(), view.clone()) { assert_eq!(previous, view); }
    json!({"marshal_hex":encoded})
}

fn scalar(value: &Value) -> Json {
    match value {
        Value::None => Json::Null,
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::Float(v) if v.is_finite() => json!(v),
        Value::Bytes(v) => json!(std::str::from_utf8(v).expect("protocol text")),
        Value::Unicode(v) => json!(v),
        Value::Tuple(v) | Value::List(v) => Json::Array(v.iter().map(scalar).collect()),
        other => panic!("unsupported scalar notification: {other:?}"),
    }
}

fn notification(packet: &Packet) -> Option<(String, Vec<Value>)> {
    let Address::Broadcast { broadcast_id, .. } = &packet.destination else { return None };
    let name = broadcast_id.as_text()?.into_owned();
    let envelope = packet.body.first()?.items()?;
    let bytes = envelope.get(1)?.as_bytes()?;
    let payload = marshal::loads(bytes).expect("notification payload");
    let tuple = payload.items().expect("notification tuple");
    assert_eq!(tuple.first().and_then(Value::as_int), Some(1));
    Some((name, tuple.get(1).and_then(Value::items).expect("notification args").to_vec()))
}

fn initial_state(client: &mut Client) -> Value {
    client.drain_destiny();
    let mut state = None;
    for (name, args) in client.outbox.iter().filter_map(notification) {
        if name != "DoDestinyUpdate" { continue; }
        let events = args[0].items().unwrap();
        for event in events {
            if event.items().and_then(|fields| fields.get(1)).and_then(Value::items)
                .and_then(|call| call.first()).and_then(Value::as_text).as_deref() == Some("SetState") {
                assert_eq!(events.len(), 1, "SetState must be a separate batch");
                assert!(state.replace(event.clone()).is_none(), "duplicate SetState");
            }
        }
    }
    state.expect("initial SetState")
}

fn destiny_event(value: &Value, packet_index: usize, batch_event_index: usize, objects: &mut BTreeMap<String, Json>) -> (Json, Json) {
    let fields = value.items().expect("destiny event tuple");
    assert_eq!(fields.len(), 2);
    let stamp = fields[0].as_int().expect("destiny stamp");
    let call = fields[1].items().expect("destiny call tuple");
    assert_eq!(call.len(), 2);
    let op = call[0].as_text().expect("destiny operation").into_owned();
    let args = call[1].items().expect("destiny args");
    let (args_json, stream) = match op.as_str() {
        "AddBalls" => {
            assert_eq!(args.len(), 1);
            let chunk = args[0].items().expect("AddBalls chunk");
            assert_eq!(chunk.len(), 3);
            let stream = hex(chunk[0].as_bytes().expect("AddBalls state bytes"));
            let args = json!([[{"bytes_hex":stream}, opaque(&chunk[1], objects), opaque(&chunk[2], objects)]]);
            (args, Some(stream))
        }
        "SetState" => {
            assert_eq!(args.len(), 1);
            (json!([opaque(&args[0], objects)]), None)
        }
        "OnSpecialFX" => {
            assert_eq!(args.len(), 14);
            let mut rendered: Vec<_> = args[..13].iter().map(scalar).collect();
            rendered.push(if args[13] == Value::None { Json::Null } else { opaque(&args[13], objects) });
            (Json::Array(rendered), None)
        }
        _ => (Json::Array(args.iter().map(scalar).collect()), None),
    };
    let batch = json!([stamp, [op, args_json]]);
    let mut event = json!({"stamp":stamp, "op":op, "args":args_json,
        "packet_index":packet_index, "batch_event_index":batch_event_index, "source":"tcp_event"});
    if let Some(stream) = stream { event["stream_hex"] = json!(stream); }
    (event, batch)
}

fn checkpoint(ball: &Ball, stamp: i64, phase: &str, event_index: Option<usize>) -> Json {
    let samples: Vec<_> = crate::space::system::tests::events::completed_segment_samples(ball)
        .into_iter().map(|(fraction, pos, vel)| {
            assert!(pos.iter().chain(vel.iter()).all(|value| value.is_finite()), "nonfinite server interpolation sample");
            json!({"fraction":fraction, "pos":pos, "vel":vel})
        }).collect();
    let mut value = json!({"stamp":stamp, "phase":phase, "id":ball.id, "mode":ball.mode,
        "pos":ball.pos, "vel":ball.vel, "samples":samples, "source":"server_expectation"});
    if matches!(ball.mode, space::MODE_WARP | space::MODE_MISSILE) {
        value["effect_stamp"] = json!(ball.effect_stamp);
    }
    if matches!(ball.mode, space::MODE_GOTO | space::MODE_WARP | space::MODE_MISSILE) {
        value["goto"] = json!(ball.goto);
    }
    if ball.mode == space::MODE_WARP {
        value["length"] = json!(ball.warp.length);
        value["min_range"] = json!(ball.warp.min_range);
        value["factor"] = json!(ball.warp.factor);
    }
    if let Some(index) = event_index { value["event_index"] = json!(index); }
    value
}
