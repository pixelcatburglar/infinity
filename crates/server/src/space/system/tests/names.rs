use super::*;

fn fields(v: &Value) -> &Value { let Value::Instance(_, s) = v else { panic!("slim instance") }; s }
fn text(v: &Value) -> &str { let Value::Bytes(b) = v else { panic!("byte string") }; std::str::from_utf8(b).expect("ASCII") }
fn typed(k: &Value, v: &Value) -> serde_json::Value {
    let (tag, value) = match v {
        Value::Int(i) => ("int", i.to_string().into()), Value::Float(f) => ("float", (*f).into()),
        Value::Unicode(s) => ("unicode", s.clone().into()), Value::None => ("none", serde_json::Value::Null),
        _ => panic!("unsupported slim field value"),
    };
    serde_json::json!([text(k), tag, value])
}

#[test]
fn committed_item_names_refresh_visible_observers_and_snapshots() {
    let (item, caller, watcher, idle) = (9_000_000_001_i64, 9_000_001, 9_000_002, 9_000_003);
    let mut s = System::new(1, Arc::default(), 1000);
    let chan = |pilot| { let (tx, rx) = mpsc::unbounded_channel(); s.registry.register_pilot(pilot, tx); rx };
    let (mut crx, mut wrx, mut irx) = (chan(caller), chan(watcher), chan(idle));
    for (id, pilot, x) in [(1, Some(caller), 0.0), (2, Some(watcher), 1.0), (3, Some(idle), 2.0)] { s.insert(ball(id, pilot, x)); }
    let _obs = [watch(&mut s, 11, 1), watch(&mut s, 12, 2)];
    s.insert({ let mut b = ball(item, None, 3.0); b.type_id = infinity_core::consts::TYPE_CAPSULE; b.slim.push(("name", Value::unicode("旧"))); b });
    let row = Value::list([Value::Int(item), Value::unicode("新ξ"), Value::None, Value::Float(0.0), Value::Float(0.0), Value::Float(0.0), Value::None]);
    s.set_item_name(item, "新ξ".into(), &row, caller);
    let (notify, args) = wrx.try_recv().expect("visible observer notified");
    assert_eq!((notify.as_str(), args.as_slice()), ("OnCfgDataChanged", [Value::str("evelocations"), row.clone()].as_slice()));
    assert!(wrx.try_recv().is_err() && crx.try_recv().is_err() && irx.try_recv().is_err());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(13, 2, tx, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    let state = super::events::initial_state(&mut rx);
    let slims = fields(&state.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0]).get("slims").unwrap().clone();
    assert_eq!(slims.items().unwrap().iter().find(|v| fields(v).get("itemID").and_then(Value::as_int) == Some(item)).and_then(|v| fields(v).get("name").cloned()), Some(Value::unicode("新ξ")));
    let event = add_balls_event(s.current_time(), s.ball(item).unwrap(), false);
    let Value::Instance(class, slim_state) = &event.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap()[1].items().unwrap()[0] else { panic!("slim") };
    let (Value::Bytes(_), Value::Dict(ordered)) = (&**class, &**slim_state) else { panic!("slim class and dict") };
    assert_eq!(ordered.iter().find(|(k, _)| k.as_text().as_deref() == Some("name")).map(|(_, v)| v), Some(&Value::unicode("新ξ")));
    println!("LABEL_CACHE_CAPTURE {}", serde_json::json!({ "id": "add-balls-name", "scenario": "add-balls", "kind": "destiny", "event": "AddBalls",
        "origin": "production-constructor", "item": item.to_string(), "system": s.id.to_string(), "name": "新ξ", "position": s.ball(item).unwrap().pos,
        "slim_class": text(class), "envelope": marshal::dumps(&event), "slim_fields": ordered.iter().map(|(k, v)| typed(k, v)).collect::<Vec<_>>() }));
    s.balls.get_mut(&item).unwrap().cloak = 1;
    s.set_item_name(item, "隐".into(), &row, caller);
    assert!(wrx.try_recv().is_err() && crx.try_recv().is_err() && irx.try_recv().is_err());
    assert_eq!(s.ball(item).unwrap().slim.iter().filter(|(k, _)| *k == "name").map(|(_, v)| v.clone()).collect::<Vec<_>>(), [Value::unicode("隐")]);
}
