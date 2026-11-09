use super::*;

fn activation(generation: u64) -> [u8; 16] {
    let mut value = [0; 16];
    value[8..].copy_from_slice(&generation.to_be_bytes());
    value
}

#[test]
fn duplicates_and_false_finish_are_consumed() {
    let mut replay = Replay::new(2);
    let a = activation(1);
    assert_eq!(replay.register(a, 1, 0), Ok(()));
    assert_eq!(replay.reserve(a, 1, 0), Ok(()));
    assert_eq!(replay.reserve(a, 1, 0), Err(Refusal::Replay));
    replay.finish(a, 1, 0, false);
    replay.finish(a, 1, 0, true);
    assert_eq!(replay.reserve(a, 1, 0), Err(Refusal::Replay));
    assert_eq!(replay.reserve(a, 1, 1), Ok(()));
}

#[test]
fn out_of_order_completion_preserves_pending_prefix() {
    let mut replay = Replay::new(3);
    let a = activation(1);
    replay.register(a, 1, 1).unwrap();
    for sequence in 1..=3 { replay.reserve(a, 1, sequence).unwrap(); }
    replay.finish(a, 1, 3, true);
    assert_eq!(replay.claim(a, 1, 1), Ok(()));
    assert_eq!(replay.claim(a, 1, 3), Err(Refusal::Replay));
    replay.finish(a, 1, 1, true);
    assert_eq!(replay.claim(a, 1, 2), Ok(()));
    assert_eq!(replay.reserve(a, 1, 3), Err(Refusal::Replay));
    replay.finish(a, 1, 2, true);
    assert_eq!(replay.reserve(a, 1, 4), Ok(()));
}

#[test]
fn capacity_never_evicts_sources_or_pending_attempts() {
    let mut replay = Replay::new(2);
    let a = activation(1);
    let b = activation(2);
    let c = activation(3);
    replay.register(a, 1, 1).unwrap();
    replay.register(b, 2, 1).unwrap();
    assert_eq!(replay.register(c, 3, 1), Err(Refusal::Full));
    replay.reserve(a, 1, 1).unwrap();
    replay.reserve(a, 1, 2).unwrap();
    assert_eq!(replay.reserve(b, 2, 1), Err(Refusal::Full));
    replay.finish(a, 1, 2, true);
    assert_eq!(replay.reserve(b, 2, 1), Err(Refusal::Full));
    replay.finish(a, 1, 1, true);
    assert_eq!(replay.reserve(b, 2, 1), Ok(()));
    assert_eq!(replay.reserve(a, 1, 1), Err(Refusal::Replay));
}

#[test]
fn registration_and_retirement_preserve_live_fences() {
    let mut replay = Replay::new(2);
    let a = activation(7);
    assert_eq!(replay.register(a, 8, 1), Err(Refusal::Invalid));
    replay.register(a, 7, 1).unwrap();
    assert_eq!(replay.register(a, 7, 1), Ok(()));
    replay.reserve(a, 7, 1).unwrap();
    assert_eq!(replay.register(a, 7, 2), Err(Refusal::Invalid));
    assert_eq!(replay.retire(a, 8), Err(Refusal::Invalid));
    assert_eq!(replay.retire(a, 7), Ok(()));
    assert_eq!(replay.register(a, 7, 1), Err(Refusal::Invalid));
    assert_eq!(replay.reserve(a, 7, 2), Err(Refusal::Invalid));
    assert_eq!(replay.claim(a, 7, 1), Ok(()));
    replay.finish(a, 7, 1, true);
    assert_eq!(replay.reserve(a, 7, 2), Err(Refusal::Invalid));
    assert_eq!(replay.reserve(activation(9), 9, 1), Err(Refusal::Invalid));
}

#[test]
fn retired_sources_release_capacity() {
    let mut replay = Replay::new(1);
    for generation in 0..5000 {
        let a = activation(generation);
        replay.register(a, generation, 1).unwrap();
        replay.reserve(a, generation, 1).unwrap();
        replay.retire(a, generation).unwrap();
        assert_eq!(replay.register(activation(generation + 1), generation + 1, 1), Err(Refusal::Full));
        replay.finish(a, generation, 1, false);
    }
}

#[test]
fn large_sequences_and_claim_are_safe() {
    let mut replay = Replay::new(2);
    let a = activation(u64::MAX);
    replay.register(a, u64::MAX, u64::MAX - 1).unwrap();
    replay.reserve(a, u64::MAX, u64::MAX).unwrap();
    assert_eq!(replay.claim(a, u64::MAX, u64::MAX), Ok(()));
    assert_eq!(replay.claim(a, u64::MAX, u64::MAX), Err(Refusal::Replay));
    replay.finish(a, u64::MAX, u64::MAX, false);
    assert_eq!(replay.reserve(a, u64::MAX, u64::MAX), Err(Refusal::Replay));
    assert_eq!(replay.reserve(a, u64::MAX, 0), Err(Refusal::Replay));
}

#[test]
fn replay_storage_type_layout() {
    use std::mem::size_of;
    println!("replay type bytes: Replay={} Source={} Node={} OptionNode={} State={}",
        size_of::<Replay>(), size_of::<Source>(), size_of::<Node>(),
        size_of::<Option<Node>>(), size_of::<State>());
}
