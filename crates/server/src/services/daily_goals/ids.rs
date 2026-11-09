//! Stable daily-goal entropy and canonical UUID identities.
//! PostgreSQL UUID text is lowercase; authored goal UUIDs also allow uppercase hex.
//! These server-defined identities do not use randomness or client numeric IDs.
use sha2::{Digest, Sha256};
use infinity_core::consts::UUID_LEN;

fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}
fn base(domain: &[u8], character: i64, start: i64, version: &str, generation: i64) -> Sha256 {
    let mut hash = Sha256::new();
    field(&mut hash, domain);
    field(&mut hash, &character.to_be_bytes());
    field(&mut hash, &start.to_be_bytes());
    field(&mut hash, version.as_bytes());
    field(&mut hash, &generation.to_be_bytes());
    hash
}

pub(crate) fn entropy(character: i64, start: i64, version: &str, generation: i64) -> [u64; 4] {
    let digest = base(b"daily-goals-entropy-v1", character, start, version, generation).finalize();
    let mut words = [0; 4];
    for (word, chunk) in words.iter_mut().zip(digest.chunks_exact(8)) {
        *word = u64::from_be_bytes(chunk.try_into().expect("SHA-256 word length"));
    }
    words
}

fn uuid(hash: Sha256) -> String {
    let digest = hash.finalize();
    crate::uuid_text::format((&digest[..UUID_LEN]).try_into().expect("SHA-256 UUID length"))
}

pub(crate) fn goal(character: i64, start: i64, version: &str, slot: &str, generation: i64) -> String {
    let mut hash = base(b"daily-goals-goal-v1", character, start, version, generation);
    field(&mut hash, slot.as_bytes());
    uuid(hash)
}

pub(crate) fn asset(character: i64, start: i64, version: &str, slot: &str, generation: i64, goal: &[u8; UUID_LEN], reward_index: i32) -> String {
    let mut hash = base(b"daily-goals-asset-v1", character, start, version, generation);
    field(&mut hash, slot.as_bytes());
    field(&mut hash, goal);
    field(&mut hash, &reward_index.to_be_bytes());
    uuid(hash)
}


#[cfg(test)] mod tests;
