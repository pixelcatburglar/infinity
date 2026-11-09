use super::*;
use sha2::{Digest, Sha256};

mod projection;
mod semantic;
mod http;

impl Default for Achievements {
    fn default() -> Self {
        Self::load(crate::retail::cdn("achievements").expect("embedded achievement catalogue"))
            .expect("valid embedded achievement catalogue")
    }
}

fn valid(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && format::achievement_data_buffer_has_identifier(bytes)
        && format::root_as_achievement_data(bytes).ok().is_some_and(|root| validate::validate(root).is_ok())
}

#[test]
fn empty_root_is_valid_and_corruption_is_rejected() {
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let root = format::AchievementData::create(&mut builder, &format::AchievementDataArgs::default());
    format::finish_achievement_data_buffer(&mut builder, root);
    let bytes = builder.finished_data().to_vec();
    assert!(valid(&bytes));
    let mut bad_magic = bytes.clone();
    bad_magic[4] ^= 1;
    assert!(!valid(&bad_magic));
    let mut bad_offset = bytes;
    bad_offset[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(!valid(&bad_offset));
    assert!(!valid(&[0; 3]));
}

#[test]
fn verifier_rejects_bad_vtable_and_utf8() {
    let mut b = flatbuffers::FlatBufferBuilder::new();
    let id = b.create_string("00000000-0000-0000-0000-000000000001");
    let category = format::Category::create(&mut b, &format::CategoryArgs { id: Some(id), ..Default::default() });
    let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(category), ..Default::default() });
    format::finish_achievement_data_buffer(&mut b, root);
    let bytes = b.finished_data().to_vec();
    assert!(valid(&bytes));
    let mut invalid_utf8 = bytes.clone();
    let pos = invalid_utf8.windows(36).position(|v| v == b"00000000-0000-0000-0000-000000000001").unwrap();
    invalid_utf8[pos] = 0xff;
    assert!(!valid(&invalid_utf8));
    let root = format::root_as_achievement_data(&bytes).unwrap();
    let id = root.goals().unwrap().id().unwrap();
    let id_offset = id.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut invalid_string = bytes.clone();
    invalid_string[id_offset - 4..id_offset].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(!valid(&invalid_string));
    let mut invalid_vtable = bytes;
    let root_pos = u32::from_le_bytes(invalid_vtable[..4].try_into().unwrap()) as usize;
    invalid_vtable[root_pos..root_pos + 4].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(!valid(&invalid_vtable));
}

#[test]
fn verifier_rejects_invalid_vector_length_and_element_offset() {
    let mut b = flatbuffers::FlatBufferBuilder::new();
    let id = b.create_string("00000000-0000-0000-0000-000000000001");
    let category = format::Category::create(&mut b, &format::CategoryArgs { id: Some(id), ..Default::default() });
    let children = b.create_vector(&[category]);
    let parent_id = b.create_string("00000000-0000-0000-0000-000000000002");
    let parent = format::Category::create(&mut b, &format::CategoryArgs { id: Some(parent_id), categories: Some(children), ..Default::default() });
    let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(parent), ..Default::default() });
    format::finish_achievement_data_buffer(&mut b, root);
    let bytes = b.finished_data();
    let root = format::root_as_achievement_data(bytes).unwrap();
    let vector = root.goals().unwrap().categories().unwrap().bytes();
    let offset = vector.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut invalid = bytes.to_vec();
    invalid[offset - 4..offset].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(!valid(&invalid));
    let mut invalid = bytes.to_vec();
    invalid[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(!valid(&invalid));
}

#[test]
fn embedded_catalogue_and_metadata_match_source() {
    let source = crate::retail::cdn("achievements").unwrap();
    assert_eq!(source.data.len(), 39552);
    assert_eq!(Sha256::digest(source.data).as_slice(), &[0x8c, 0x08, 0xe4, 0xd8, 0x41, 0x49, 0x01, 0x69, 0x03, 0xe6, 0xf5, 0xa2, 0xed, 0x61, 0xa4, 0x5d, 0x25, 0xdf, 0x9a, 0xbb, 0x70, 0xf5, 0x5a, 0x98, 0xf8, 0x5f, 0x12, 0xc2, 0xbd, 0xdf, 0x62, 0x01]);
    let catalog = Achievements::load(source).unwrap();
    assert_eq!(catalog.checkpoint_metadata().version, (1, 0, 7));
    let methods = catalog.catalogue().contribution_methods().unwrap();
    let mut modes = [0usize; 4];
    for method in methods { modes[method.progress_type().0 as usize] += 1; }
    assert_eq!(modes, [0, 26, 1, 5]);
    fn count(category: format::Category<'_>, totals: &mut [usize; 10]) {
        totals[0] += 1;
        if let Some(ms) = category.milestones() {
            for m in ms {
                totals[2] += 1;
                if let Some(rs) = m.rewards() { for r in rs { totals[3] += 1; totals[3 + r.payload_type().0 as usize] += 1; } }
            }
        }
        if let Some(children) = category.achievements() {
            for a in children {
                totals[1] += 1;
                if a.archived() { totals[9] += 1; }
                if a.release_date_unix_ms().is_some() || a.expiration_date_unix_ms().is_some() { totals[8] += 1; }
                if let Some(ms) = a.milestones() {
                    for m in ms {
                        totals[2] += 1;
                        if let Some(rs) = m.rewards() { for r in rs { totals[3] += 1; totals[3 + r.payload_type().0 as usize] += 1; } }
                    }
                }
            }
        }
        if let Some(children) = category.categories() { for c in children { count(c, totals); } }
    }
    let root = catalog.catalogue();
    let mut totals = [0; 10];
    for category in [root.goals(), root.challenges(), root.time_limited()].into_iter().flatten() { count(category, &mut totals); }
    assert_eq!(totals, [12, 38, 87, 73, 0, 10, 20, 43, 4, 1]);
    assert_eq!(catalog.checkpoint_metadata().crc, 660508158);
    assert_eq!(catalog.root.contribution_methods().unwrap().len(), 32);
}
