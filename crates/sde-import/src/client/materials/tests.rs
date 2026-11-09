use super::decode;
use serde_json::json;

fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn wrap(payload: Vec<u8>) -> Vec<u8> {
    let mut raw = vec![0; 32];
    put64(&mut raw, 24, payload.len() as u64);
    raw.extend(payload);
    raw
}
fn header(bytes: &mut [u8]) {
    put64(bytes, 0, 24);
    put64(bytes, 8, 1);
    put64(bytes, 16, 1);
    put64(bytes, 24, 40);
    put64(bytes, 32, 1);
    put64(bytes, 40, 7);
}
fn materials() -> Vec<u8> {
    let mut p = vec![0; 108];
    header(&mut p);
    put64(&mut p, 48, 80);
    put64(&mut p, 56, 96);
    put64(&mut p, 64, 1);
    put64(&mut p, 72, 1);
    put32(&mut p, 80, 12);
    put32(&mut p, 84, 0);
    put64(&mut p, 88, 1);
    put32(&mut p, 96, 13);
    put32(&mut p, 100, 9);
    put32(&mut p, 104, 2);
    wrap(p)
}
fn skills() -> Vec<u8> {
    let mut p = vec![0; 120];
    header(&mut p);
    put64(&mut p, 48, 72);
    put64(&mut p, 56, 2);
    put64(&mut p, 64, 2);
    put64(&mut p, 72, 96);
    put64(&mut p, 80, 112);
    put64(&mut p, 88, 1);
    put32(&mut p, 96, 4);
    put32(&mut p, 100, 0);
    put64(&mut p, 104, 1);
    put32(&mut p, 112, 5);
    put32(&mut p, 116, 5);
    wrap(p)
}
#[test]
fn material_projection_preserves_random_triples_and_zero() {
    assert_eq!(decode("typematerials", &materials()).unwrap(), vec![json!({"_key":7,"materials":[{"materialTypeID":12,"quantity":0}],"randomizedMaterials":[{"materialTypeID":13,"quantityMax":9,"quantityMin":2}]})]);
}
#[test]
fn optional_random_empty_and_absent_are_distinct() {
    let mut raw = materials();
    put64(&mut raw, 32 + 88, 0);
    assert_eq!(decode("typematerials", &raw).unwrap()[0]["randomizedMaterials"], json!([]));
    put64(&mut raw, 32 + 64, 0);
    assert!(decode("typematerials", &raw).unwrap()[0].get("randomizedMaterials").is_none());
    put64(&mut raw, 32 + 72, 0);
    assert_eq!(decode("typematerials", &raw).unwrap()[0]["materials"], json!([]));
}
#[test]
fn nested_skills_preserve_map_levels() {
    assert_eq!(decode("requiredskillsfortypes", &skills()).unwrap(), vec![json!({"_key":7,"4":0,"5":5})]);
}
#[test]
fn empty_map_and_missing_rows_are_distinct() {
    let mut raw = skills();
    put64(&mut raw, 32 + 56, 0);
    put64(&mut raw, 32 + 88, 0);
    put64(&mut raw, 32 + 104, 0);
    assert_eq!(decode("requiredskillsfortypes", &raw).unwrap(), vec![json!({"_key":7})]);
    put64(&mut raw, 32 + 8, 0);
    put64(&mut raw, 32 + 32, 0);
    assert!(decode("requiredskillsfortypes", &raw).unwrap().is_empty());
}
#[test]
fn malformed_and_overflow_rejected() {
    let mut raw = materials();
    raw.pop();
    assert!(decode("typematerials", &raw).is_err());
    let mut raw = materials();
    put64(&mut raw, 32 + 72, u64::MAX);
    assert!(decode("typematerials", &raw).is_err());
    let mut raw = materials();
    put64(&mut raw, 32 + 48, 2);
    assert!(decode("typematerials", &raw).is_err());
    let mut raw = materials();
    put64(&mut raw, 32 + 64, 2);
    assert!(decode("typematerials", &raw).is_err());
    assert!(decode("unknown", &materials()).is_err());
}
#[test]
fn nested_duplicate_misbucket_and_invalid_level_rejected() {
    let mut raw = skills();
    put32(&mut raw, 32 + 112, 4);
    assert!(decode("requiredskillsfortypes", &raw).is_err());
    let mut raw = skills();
    put32(&mut raw, 32 + 100, 6);
    assert!(decode("requiredskillsfortypes", &raw).is_err());
    let mut raw = skills();
    put64(&mut raw, 32 + 56, 3);
    assert!(decode("requiredskillsfortypes", &raw).is_err());
    let mut raw = skills();
    put64(&mut raw, 32 + 64, 0);
    assert!(decode("requiredskillsfortypes", &raw).is_err());
}
#[test]
fn duplicate_skills_in_same_bucket_rejected() {
    let mut raw = skills();
    raw.extend([0; 8]);
    let len = raw.len() - 32;
    put64(&mut raw, 24, len as u64);
    put64(&mut raw, 32 + 56, 3);
    put64(&mut raw, 32 + 104, 2);
    put32(&mut raw, 32 + 120, 5);
    put32(&mut raw, 32 + 124, 1);
    assert!(decode("requiredskillsfortypes", &raw).is_err());
}
