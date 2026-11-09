use super::*;

#[test]
fn unsupported_arguments_are_exact() {
    assert!(valid_args(&["unsupported".into()]));
    for args in [&[][..], &["unsupported", "extra"][..], &["other"][..], &["Unsupported"][..]] {
        assert!(!valid_args(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()));
    }
}

#[test]
fn unsupported_output_includes_each_pair_in_snapshot_order() {
    assert_eq!(messages::gm::gm_effects_unsupported(&[]), "尚未记录未支持的效果。");
    let effects = vec![("alpha".to_string(), 42), ("beta".to_string(), 7)];
    assert_eq!(messages::gm::gm_effects_unsupported(&effects), "alpha（类型 ID：42）\nbeta（类型 ID：7）");
}
