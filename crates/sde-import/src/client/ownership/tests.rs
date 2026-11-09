use super::*;

fn put(raw: &mut [u8], at: usize, value: u64) { raw[at..at + 8].copy_from_slice(&value.to_le_bytes()); }

#[test]
fn synthetic_bucket_counts_and_exact_unknown_bytes() {
    let mut raw = vec![0; 104];
    put(&mut raw, 24, 72);
    put(&mut raw, 32, 24);
    put(&mut raw, 40, 1);
    put(&mut raw, 48, 1);
    put(&mut raw, 56, 40);
    put(&mut raw, 64, 1);
    raw[72..76].copy_from_slice(&7u32.to_le_bytes());
    raw[76] = 0xab;
    let result = analyze("groups", &raw).unwrap();
    assert_eq!((result.source_bytes, result.owned_bytes, result.unknown_bytes, result.unowned_bytes), (104, 52, 52, 0));
    let body = result.intervals.iter().find(|r| r.label.starts_with("record body")).unwrap();
    assert_eq!((body.offset, body.length, body.owner_keys.as_slice()), (76, 28, &[7][..]));
    assert!(body.hex.as_ref().unwrap().starts_with("ab"));
}

#[test]
fn overlap_sharing_and_gaps_are_disjoint() {
    let raw = [1u8; 20];
    let mut c = Vec::new();
    claim(&mut c, &raw, 0, 4, Kind::Unknown, Some(1), "dogma vector").unwrap();
    claim(&mut c, &raw, 0, 4, Kind::Unknown, Some(2), "dogma vector").unwrap();
    claim(&mut c, &raw, 8, 6, Kind::Unknown, Some(1), "dogma vector").unwrap();
    claim(&mut c, &raw, 10, 6, Kind::Unknown, Some(2), "dogma vector").unwrap();
    let r = finish(&raw, &c);
    assert_eq!(r.intervals[0].owner_keys, vec![1, 2]);
    assert_eq!(r.intervals[0].label, "shared dogma vector");
    assert!(r.intervals.iter().any(|x| x.offset == 10 && x.length == 4 && x.kind == Kind::Unowned && x.label == "overlapping ownership claims"));
    assert!(r.intervals.iter().any(|x| x.offset == 4 && x.length == 4 && x.kind == Kind::Unowned));
    assert_eq!(r.owned_bytes + r.unknown_bytes + r.unowned_bytes, raw.len());
}

#[test]
fn alignment_requires_keyed_allocation_zero_and_boundary() {
    let raw = [0u8; 24];
    let mut c = Vec::new();
    claim(&mut c, &raw, 0, 5, Kind::Unknown, Some(4), "pointed string").unwrap();
    claim(&mut c, &raw, 8, 5, Kind::Unknown, None, "pointed string").unwrap();
    claim(&mut c, &raw, 16, 4, Kind::Unknown, Some(5), "record body: semantic ownership not proven").unwrap();
    let r = finish(&raw, &c);
    assert_eq!(r.intervals[1].label, "inferred allocation alignment padding");
    assert_eq!(r.intervals[1].owner_keys, vec![4]);
    assert_eq!(r.intervals[3].kind, Kind::Unowned);
    assert_eq!(r.intervals[5].kind, Kind::Unowned);
    let mut nonzero = raw;
    nonzero[5] = 1;
    assert_eq!(finish(&nonzero, &c).intervals[1].kind, Kind::Unowned);
}

#[test]
fn bounds_and_unsupported_inputs_fail_closed() {
    assert!(analyze("groups", &[0; 31]).is_err());
    assert!(analyze("unknown", &[]).is_err());
    let mut c = Vec::new();
    assert!(claim(&mut c, &[0; 8], usize::MAX, 2, Kind::Owned, None, "x").is_err());
    assert!(claim(&mut c, &[0; 8], 7, 2, Kind::Owned, None, "x").is_err());
    let raw = [0; 16];
    let reader = Reader::new(&raw);
    assert!(vector(&mut c, &raw, &reader, 0, 8, 1, "dogma vector", None).is_err());
}

#[test]
fn many_overlaps_have_bounded_endpoint_output() {
    let n = 4000;
    let raw = vec![1u8; n * 2 + 1];
    let mut claims = Vec::new();
    for i in 0..n {
        claim(&mut claims, &raw, i, n + 1, Kind::Unknown, Some(i as u64), "dogma vector").unwrap();
    }
    let result = finish(&raw, &claims);
    // Each claim contributes exactly two endpoints, irrespective of overlap depth.
    assert!(result.intervals.len() <= claims.len() * 2 + 1);
    assert_eq!(result.unowned_bytes, n * 2 - 1);
    assert_eq!(result.owned_bytes + result.unknown_bytes + result.unowned_bytes, raw.len());
}

fn types_with_two_buckets() -> Vec<u8> {
    let mut raw = vec![0; 408];
    put(&mut raw, 24, 376);
    put(&mut raw, 32, 24);
    put(&mut raw, 40, 2);
    put(&mut raw, 48, 2);
    put(&mut raw, 56, 48);
    put(&mut raw, 64, 216);
    put(&mut raw, 72, 1);
    put(&mut raw, 80 + 24, 208);
    put(&mut raw, 240, 1);
    raw[248..252].copy_from_slice(&1u32.to_le_bytes());
    put(&mut raw, 248 + 24, 376);
    raw
}

#[test]
fn designer_vector_must_fit_own_bucket() {
    let raw = types_with_two_buckets();
    let result = analyze("types", &raw).unwrap();
    assert_eq!(result.unowned_bytes, 0);
    assert_eq!(result.owned_bytes + result.unknown_bytes + result.unowned_bytes, raw.len());
    // The first prefix is exactly at low and its empty vector ends at high.
    assert!(result.intervals.iter().any(|x| x.offset == 232 && x.length == 8 && x.owner_keys == vec![0]));
    let mut bad = raw.clone();
    put(&mut bad, 104, 376); // second bucket's allocation
    assert!(analyze("types", &bad).is_err());
    put(&mut bad, 104, 200); // prefix inside the first record body
    assert!(analyze("types", &bad).is_err());
    put(&mut bad, 104, 212); // pointer past first bucket's high boundary
    assert!(analyze("types", &bad).is_err());
    let mut bad = raw;
    put(&mut bad, 232, 1); // nonempty vector extends beyond high
    assert!(analyze("types", &bad).is_err());
}
