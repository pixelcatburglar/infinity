use super::decode;
fn put32(b: &mut [u8], p: usize, v: u32) { b[p..p+4].copy_from_slice(&v.to_le_bytes()); }
fn put64(b: &mut [u8], p: usize, v: u64) { b[p..p+8].copy_from_slice(&v.to_le_bytes()); }
fn fixture(stride: usize, extra: usize) -> Vec<u8> {
    let mut b = vec![0;32+40+stride+extra]; let n = (b.len()-32) as u64;
    put64(&mut b,24,n); put64(&mut b,32,24); put64(&mut b,40,1);
    put64(&mut b,48,1); put64(&mut b,56,40); put64(&mut b,64,1);
    put32(&mut b,72,7); b
}
#[test]
fn categories_optional_empty_string_and_identity() {
    let mut b=fixture(40,9); let o=72;
    put32(&mut b,o+16,7); put32(&mut b,o+32,3); put64(&mut b,o+8,88);
    let r=decode("categories",&b).unwrap();
    assert_eq!(r[0]["iconID"],0); assert_eq!(r[0]["sofBuildClass"],"");
    put32(&mut b,o+32,0); let r=decode("categories",&b).unwrap();
    assert!(r[0].get("iconID").is_none()); assert!(r[0].get("sofBuildClass").is_none());
    b[o+28]=2; assert!(decode("categories",&b).is_err());
    b[o+28]=0; put32(&mut b,o+16,8); assert!(decode("categories",&b).is_err());
}
#[test]
fn metagroup_color_and_pointer_validation() {
    let mut b=fixture(48,9); let o=72;
    put32(&mut b,o+44,9); put64(&mut b,o+8,96); put32(&mut b,o+16,0.1f32.to_bits());
    let r=decode("metagroups",&b).unwrap();
    assert_eq!(r[0]["color"][0].as_f64().unwrap(),f64::from(0.1f32));
    assert_eq!(r[0]["iconSuffix"],"");
    put32(&mut b,o+16,f32::INFINITY.to_bits()); assert!(decode("metagroups",&b).is_err());
    put32(&mut b,o+16,0); put64(&mut b,o+8,u64::MAX); assert!(decode("metagroups",&b).is_err());
}
#[test]
fn market_parent_presence_and_invalid_flags() {
    let mut b=fixture(28,0); let o=72;
    assert!(decode("marketgroups",&b).unwrap()[0].get("parentGroupID").is_none());
    put32(&mut b,o+24,4); assert_eq!(decode("marketgroups",&b).unwrap()[0]["parentGroupID"],0);
    put32(&mut b,o+20,2); assert!(decode("marketgroups",&b).is_err());
    put32(&mut b,o+20,0); put32(&mut b,o+24,8); assert!(decode("marketgroups",&b).is_err());
    assert!(decode("marketgroups",&b[..b.len()-1]).is_err());
}
