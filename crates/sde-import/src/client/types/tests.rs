use super::decode;

fn put32(b: &mut [u8], p: usize, v: u32) { b[p..p+4].copy_from_slice(&v.to_le_bytes()); }
fn put64(b: &mut [u8], p: usize, v: u64) { b[p..p+8].copy_from_slice(&v.to_le_bytes()); }
fn fixture(stride: usize, extra: usize) -> Vec<u8> {
    let mut b = vec![0; 32+40+stride+extra];
    let n = (b.len()-32) as u64;
    put64(&mut b, 24, n); put64(&mut b, 32, 24); put64(&mut b, 40, 1);
    put64(&mut b, 48, 1); put64(&mut b, 56, 40); put64(&mut b, 64, 1);
    b
}
#[test]
fn group_optionals_and_invalid_values() {
    let mut b = fixture(32,0); let o = 72;
    put32(&mut b,o,7); put32(&mut b,o+8,7); put32(&mut b,o+28,0x37);
    let r = decode("groups",&b).unwrap();
    assert!(r[0].get("iconID").is_none());
    put32(&mut b,o+28,0x3f);
    assert_eq!(decode("groups",&b).unwrap()[0]["iconID"],0);
    b[o+23]=2; assert!(decode("groups",&b).is_err());
    b[o+23]=0; put32(&mut b,o+28,0x7f); assert!(decode("groups",&b).is_err());
    put32(&mut b,o+28,0x37); put32(&mut b,o+8,8); assert!(decode("groups",&b).is_err());
    assert!(decode("groups",&b[..b.len()-1]).is_err());
}
#[test]
fn type_list_presence_precision_and_rejections() {
    let mut b = fixture(152,8); let o = 72;
    let required = (1<<0)|(1<<1)|(1<<11)|(1<<14)|(1<<15)|(1<<19)|(1<<23);
    put32(&mut b,o,7); put32(&mut b,o+128,7); put32(&mut b,o+148,required);
    put64(&mut b,o+24,200); put64(&mut b,o+8,1.25f64.to_bits());
    let r = decode("types",&b).unwrap();
    assert_eq!(r[0]["basePrice"],1.25); assert!(r[0].get("designerIDs").is_none());
    put32(&mut b,o+148,required | (1<<4) | (1<<2));
    let r = decode("types",&b).unwrap();
    assert_eq!(r[0]["designerIDs"],serde_json::json!([])); assert_eq!(r[0]["certificateTemplate"],0);
    put64(&mut b,o+8,f64::NAN.to_bits()); assert!(decode("types",&b).is_err());
    put64(&mut b,o+8,0); put64(&mut b,o+24,u64::MAX); assert!(decode("types",&b).is_err());
    put64(&mut b,o+24,200); put32(&mut b,o+148,required | (1<<16)); assert!(decode("types",&b).is_err());
}
