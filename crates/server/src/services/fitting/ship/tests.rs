use super::{assembly_source_change, assembly_step};
use infinity_core::consts::{IX_SINGLETON, IX_STACK_SIZE};
use marshal::Value;

mod spawn;

#[test]
fn repeated_assembly_conserves_hulls_and_stops_after_conversion() {
    let first = assembly_step(3, None).unwrap();
    assert_eq!(first, (2, true, 3));
    let second = assembly_step(3, Some(first.0)).unwrap();
    assert_eq!(second, (1, true, 2));
    let last = assembly_step(3, Some(second.0)).unwrap();
    assert_eq!(last, (0, false, 1));
    assert_eq!(assembly_step(3, Some(last.0)), None);
    assert_eq!(assembly_step(1, None), Some((0, false, 1)));
    for quantity in [i64::from(i32::MAX) + 1, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
        assert_eq!(assembly_step(quantity, None), Some((quantity - 1, true, quantity)));
        assert!(matches!(assembly_source_change(quantity, quantity - 1).as_slice(), [(IX_STACK_SIZE, Value::Int(old))] if *old == quantity));
    }
}

#[test]
fn repeated_assembly_source_change_uses_original_quantity_and_final_state() {
    let mut remaining = None;
    for _ in 0..2 {
        remaining = Some(assembly_step(3, remaining).unwrap().0);
    }
    let partial = assembly_source_change(3, remaining.unwrap());
    assert!(matches!(partial.as_slice(), [(IX_STACK_SIZE, Value::Int(3))]));

    remaining = Some(assembly_step(3, remaining).unwrap().0);
    let converted = assembly_source_change(3, remaining.unwrap());
    assert!(matches!(converted.as_slice(), [(IX_STACK_SIZE, Value::Int(3)), (IX_SINGLETON, Value::Bool(false))]));
}
