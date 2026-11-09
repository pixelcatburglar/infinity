use super::{bucket_bounds, bucket_records, payload, Reader};

fn put(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn wrapper_and_checked_reads() {
    let mut raw = vec![0; 40];
    put(&mut raw, 24, 8);
    assert_eq!(payload(&raw).unwrap().data.len(), 8);
    put(&mut raw, 24, 9);
    assert!(payload(&raw).is_err());
    let reader = Reader::new(&raw);
    assert!(reader.slice(usize::MAX, 2).is_err());
    assert!(reader.u64(39).is_err());
    assert!(Reader::new(&f64::NAN.to_le_bytes()).f64(0).is_err());
}

#[test]
fn string_requires_utf8_terminator_and_bounds() {
    let mut data = vec![0; 12];
    put(&mut data, 0, 3);
    data[8..11].copy_from_slice(b"abc");
    assert_eq!(Reader::new(&data).string(8).unwrap(), "abc");
    data[11] = 1;
    assert!(Reader::new(&data).string(8).is_err());
    data[11] = 0;
    data[8] = 0xff;
    assert!(Reader::new(&data).string(8).is_err());
}

#[test]
fn bucket_rejects_duplicates_misbuckets_and_bad_counts() {
    let mut data = vec![0; 64];
    put(&mut data, 0, 24);
    put(&mut data, 8, 2);
    put(&mut data, 16, 1);
    put(&mut data, 24, 40);
    put(&mut data, 32, 2);
    data[40..44].copy_from_slice(&1u32.to_le_bytes());
    data[44..48].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(bucket_records(&Reader::new(&data), 4, 4).unwrap().len(), 2);
    data[44..48].copy_from_slice(&1u32.to_le_bytes());
    assert!(bucket_records(&Reader::new(&data), 4, 4).is_err());
    put(&mut data, 32, 100);
    assert!(bucket_records(&Reader::new(&data), 4, 4).is_err());
    put(&mut data, 16, 2);
    put(&mut data, 24, 48);
    put(&mut data, 32, 60);
    put(&mut data, 40, 1);
    data[48..52].copy_from_slice(&1u32.to_le_bytes());
    put(&mut data, 52, 1);
    assert!(bucket_records(&Reader::new(&data), 4, 4).is_err());
}

#[test]
fn bucket_bounds_exclude_records_and_next_header() {
    let mut data = vec![0; 96];
    put(&mut data, 0, 24);
    put(&mut data, 16, 2);
    put(&mut data, 24, 48);
    put(&mut data, 32, 80);
    put(&mut data, 40, 2);
    put(&mut data, 72, 1);
    assert_eq!(bucket_bounds(&Reader::new(&data), 4, 0).unwrap(), (56, 72));
    assert_eq!(bucket_bounds(&Reader::new(&data), 4, 1).unwrap(), (84, 96));
    assert!(bucket_bounds(&Reader::new(&data), 0, 0).is_err());
    put(&mut data, 40, u64::MAX);
    assert!(bucket_bounds(&Reader::new(&data), 4, 0).is_err());
    put(&mut data, 16, 0);
    assert!(bucket_bounds(&Reader::new(&data), 4, 0).is_err());
}
