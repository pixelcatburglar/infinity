use super::*;
fn table(bytes: &[u8]) -> TableSnapshot {
    let mut digest = Sha256::new(); add_record(&mut digest, bytes);
    TableSnapshot { columns: vec![Column { name: "value".into(), data_type: "double precision".into(), not_null: false }], rows: 1, sha256: hex(&digest.finalize()) }
}
#[test]
fn binary_float_bits_and_schema_changes_fail() {
    let expected = table(&0.0f64.to_bits().to_be_bytes());
    let signed_zero = table(&(-0.0f64).to_bits().to_be_bytes());
    assert_ne!(expected.sha256, signed_zero.sha256);
    let mut out = Readback::default();
    compare_table(&mut out, "other", &expected, Some(&signed_zero));
    assert_eq!(out.unmigrated["other"].status, "failed");
    assert_eq!(out.differences[0].path, "/sha256");
    let mut changed = expected.clone(); changed.columns[0].data_type = "numeric".into();
    compare_table(&mut out, "other", &expected, Some(&changed));
    assert!(out.differences.iter().any(|d| d.path == "/columns"));
}
#[test]
fn complete_table_set_and_missing_proof() {
    let baseline = UnmigratedSnapshot { tables: ["old".to_string(), "same".to_string()].into_iter().collect(), unmigrated: BTreeMap::new() };
    let actual = ["new".to_string(), "same".to_string()].into_iter().collect();
    let mut out = Readback::default(); compare_names(&mut out, &baseline, &actual);
    assert_eq!(out.differences.len(), 2);
    assert!(out.differences.iter().any(|d| d.table == "old" && !d.actual_present));
    assert!(out.differences.iter().any(|d| d.table == "new" && !d.expected_present));
    compare_table(&mut out, "old", &table(&[1]), None);
    assert_eq!(out.unmigrated["old"].status, "failed");
}
#[test]
fn record_length_framing_prevents_boundary_collisions() {
    let mut a = Sha256::new(); add_record(&mut a, b"ab"); add_record(&mut a, b"c");
    let mut b = Sha256::new(); add_record(&mut b, b"a"); add_record(&mut b, b"bc");
    assert_ne!(a.finalize(), b.finalize());
}
