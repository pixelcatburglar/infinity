use super::{discover, read_input, recheck};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn discovery_rejects_ambiguity_and_detects_replacement() {
    let root = std::env::temp_dir().join(format!("sde-input-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(root.join("StaticData")).unwrap();
    fs::write(root.join("StaticData/Types.fsdbinary"), b"original").unwrap();
    assert!(discover(&root, "staticdata/types.fsdbinary").is_ok());
    let input = read_input(&root, "types", "staticdata/types.fsdbinary").unwrap();
    fs::write(root.join("StaticData/Types.fsdbinary"), b"replaced").unwrap();
    assert_eq!(input.bytes.as_slice(), b"original");
    assert!(recheck(&input).is_err());
    fs::write(root.join("StaticData/types.fsdbinary"), b"other").unwrap();
    assert!(discover(&root, "staticdata/types.fsdbinary").is_err());
    assert!(discover(&root, "staticdata/missing").is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn discovery_continues_after_missing_resource_without_decoding() {
    let root = std::env::temp_dir().join(format!("sde-discovery-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let missing = &crate::client::families::FAMILIES[1];
    for family in crate::client::families::FAMILIES {
        if family.name == missing.name { continue }
        let path = root.join(family.resource);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"invalid decoder input").unwrap();
    }
    let mut inputs = Vec::new();
    let mut reports = crate::report::manifest::Reports::default();
    let manifest = crate::initial_manifest(crate::report::manifest::new_run_id());
    let run = crate::report::manifest::ReportRun::begin(&root.join("report"), &manifest).unwrap();
    let error = crate::client::prepare::decode_inputs(&root, &mut reports, &mut inputs, &run).unwrap_err();
    let context = error.downcast_ref::<crate::client::prepare::FamilyContext>().unwrap();
    assert_eq!(context.family, missing.name);
    assert_eq!(context.stage, "read");
    assert_eq!(inputs.len(), crate::client::families::FAMILIES.len() - 1);
    assert!(inputs.iter().all(|input| input.decoded_records.is_none() && !input.sha256.is_empty()));
    fs::remove_dir_all(root).unwrap();
}
