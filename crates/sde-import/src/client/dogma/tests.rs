use super::decode;
use serde_json::json;

// One bucket; record begins at payload offset 40. All bytes are synthetic.
fn fixture(stride: usize) -> Vec<u8> {
    let mut b = vec![0; 32 + 40 + stride];
    set64(&mut b, 32, 24);
    set64(&mut b, 40, 1);
    set64(&mut b, 48, 1);
    set64(&mut b, 56, 40);
    set64(&mut b, 64, 1);
    set64(&mut b, 72, 7);
    finish(&mut b);
    b
}
fn set64(b: &mut [u8], p: usize, v: u64) { b[p..p + 8].copy_from_slice(&v.to_le_bytes()); }
fn set32(b: &mut [u8], p: usize, v: u32) { b[p..p + 4].copy_from_slice(&v.to_le_bytes()); }
fn finish(b: &mut [u8]) { let n = (b.len() - 32) as u64; set64(b, 24, n); }
fn append_string(b: &mut Vec<u8>, s: &str) -> u64 {
    let ptr = (b.len() + 8 - 32) as u64;
    b.extend_from_slice(&(s.len() as u64).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
    b.push(0);
    finish(b);
    ptr
}
fn append_list(b: &mut Vec<u8>, count: u64, data: &[u8]) -> u64 {
    let ptr = (b.len() + 8 - 32) as u64;
    b.extend_from_slice(&count.to_le_bytes());
    b.extend_from_slice(data);
    finish(b);
    ptr
}

#[test]
fn attribute_presence_and_exact_f32() {
    let mut b = fixture(80);
    let name = append_string(&mut b, "name");
    let description = append_string(&mut b, "");
    set64(&mut b, 80, description);
    set64(&mut b, 88, name);
    set32(&mut b, 96, 7);
    set32(&mut b, 112, 0.1f32.to_bits());
    set32(&mut b, 148, 1 | 4 | 16);
    let rows = decode("dogmaattributes", &b).unwrap();
    assert_eq!(rows[0]["description"], "");
    assert_eq!(rows[0]["categoryID"], 0);
    assert!(rows[0].get("unitID").is_none());
    assert_eq!(rows[0]["defaultValue"].as_f64().unwrap(), f64::from(0.1f32));
    set32(&mut b, 112, f32::NAN.to_bits());
    assert!(decode("dogmaattributes", &b).is_err());
    set32(&mut b, 148, 1 << 12);
    assert!(decode("dogmaattributes", &b).is_err());
    set32(&mut b, 148, 0);
    b[144] = 2;
    assert!(decode("dogmaattributes", &b).is_err());
    b[144] = 0;
    set32(&mut b, 96, 8);
    assert!(decode("dogmaattributes", &b).is_err());
}

#[test]
fn category_and_unit_optionals_and_flags() {
    let mut b = fixture(32);
    let name = append_string(&mut b, "unit");
    set64(&mut b, 80, name);
    set64(&mut b, 96, 3);
    let row = &decode("dogmaunits", &b).unwrap()[0];
    assert_eq!(row["descriptionID"], 0);
    assert_eq!(row["displayNameID"], 0);
    set64(&mut b, 96, 4);
    assert!(decode("dogmaunits", &b).is_err());
    set64(&mut b, 88, name);
    set64(&mut b, 96, 0);
    let row = &decode("dogmaattributecategories", &b).unwrap()[0];
    assert!(row.get("description").is_none());
    set64(&mut b, 96, 1);
    assert_eq!(decode("dogmaattributecategories", &b).unwrap()[0]["description"], "unit");
    set64(&mut b, 96, 2);
    assert!(decode("dogmaattributecategories", &b).is_err());
}

#[test]
fn type_dogma_order_duplicates_and_numeric_default() {
    let mut b = fixture(24);
    let mut attrs = vec![0; 32];
    set64(&mut attrs, 0, 0.1f64.to_bits());
    set32(&mut attrs, 8, 12);
    set64(&mut attrs, 16, (-0.0f64).to_bits());
    set32(&mut attrs, 24, 12);
    let a = append_list(&mut b, 2, &attrs);
    let e = append_list(&mut b, 2, &[9,0,0,0,1,0,0,0, 9,0,0,0,0,0,0,0]);
    set64(&mut b, 80, a);
    set64(&mut b, 88, e);
    let row = &decode("typedogma", &b).unwrap()[0];
    assert_eq!(row["dogmaAttributes"][0], json!({"attributeID":12,"value":0.1}));
    assert_eq!(row["dogmaAttributes"][1]["value"].as_f64().unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(row["dogmaEffects"], json!([{ "effectID":9,"isDefault":1 },{ "effectID":9,"isDefault":0 }]));
    b[32 + e as usize + 4] = 2;
    assert!(decode("typedogma", &b).is_err());
    b[32 + e as usize + 4] = 1;
    set64(&mut b, 32 + a as usize, f64::INFINITY.to_bits());
    assert!(decode("typedogma", &b).is_err());
    set64(&mut b, 32 + a as usize - 8, u64::MAX);
    assert!(decode("typedogma", &b).is_err());
    set64(&mut b, 80, 0);
    assert!(decode("typedogma", &b).is_err());
}

#[test]
fn effects_nested_signed_and_empty_optionals() {
    let mut b = fixture(112);
    let name = append_string(&mut b, "effect");
    let domain = append_string(&mut b, "ship");
    let func = append_string(&mut b, "func");
    let mut modifier = vec![0; 48];
    set64(&mut modifier, 0, domain);
    set64(&mut modifier, 8, func);
    set32(&mut modifier, 32, u32::MAX);
    set64(&mut modifier, 40, 0x14);
    let list = append_list(&mut b, 1, &modifier);
    set64(&mut b, 80, name);
    set64(&mut b, 96, list);
    set32(&mut b, 136, 7);
    set32(&mut b, 180, 512);
    let row = &decode("dogmaeffects", &b).unwrap()[0];
    assert_eq!(row["modifierInfo"][0]["operation"], -1);
    assert_eq!(row["modifierInfo"][0]["modifiedAttributeID"], 0);
    assert!(row["modifierInfo"][0].get("groupID").is_none());
    assert!(row.get("guid").is_none());
    set64(&mut b, 32 + list as usize + 40, 64);
    assert!(decode("dogmaeffects", &b).is_err());
    set64(&mut b, 32 + list as usize + 40, 0);
    set32(&mut b, 180, 1 << 16);
    assert!(decode("dogmaeffects", &b).is_err());
    set32(&mut b, 180, 512);
    b[172] = 3;
    assert!(decode("dogmaeffects", &b).is_err());
    b[172] = 0;
    let empty = append_string(&mut b, "");
    set64(&mut b, 32 + list as usize, empty);

    assert!(decode("dogmaeffects", &b).is_err());
    set64(&mut b, 32 + list as usize - 8, 0);
    assert_eq!(decode("dogmaeffects", &b).unwrap()[0]["modifierInfo"], json!([]));
}

#[test]
fn malformed_wrapper_bucket_and_strings_fail() {
    assert!(decode("unknown", &[]).is_err());
    assert!(decode("dogmaunits", &[]).is_err());
    let mut b = fixture(32);
    set64(&mut b, 80, u64::MAX);
    assert!(decode("dogmaunits", &b).is_err());
    let name = append_string(&mut b, "a");
    set64(&mut b, 80, name);
    *b.last_mut().unwrap() = 1;
    assert!(decode("dogmaunits", &b).is_err());
    *b.last_mut().unwrap() = 0;
    set64(&mut b, 40, 2);
    assert!(decode("dogmaunits", &b).is_err());
    set64(&mut b, 40, 1);
    set64(&mut b, 56, u64::MAX);
    assert!(decode("dogmaunits", &b).is_err());
    set64(&mut b, 56, 40);
    b.pop();
    assert!(decode("dogmaunits", &b).is_err());
}
#[test]
fn duplicate_and_misbucketed_keys_fail() {
    let mut b = fixture(64);
    set64(&mut b, 40, 2);
    set64(&mut b, 64, 2);
    set64(&mut b, 104, 7);
    assert!(decode("dogmaunits", &b).is_err());

    // Two buckets, first empty, second contains key 8 (which belongs in bucket 0).
    let mut b = vec![0; 32 + 88];
    set64(&mut b, 32, 24);
    set64(&mut b, 40, 1);
    set64(&mut b, 48, 2);
    set64(&mut b, 56, 48);
    set64(&mut b, 64, 56);
    set64(&mut b, 72, 0);
    set64(&mut b, 80, 1);
    set64(&mut b, 88, 8);
    finish(&mut b);
    assert!(decode("dogmaunits", &b).is_err());
}
