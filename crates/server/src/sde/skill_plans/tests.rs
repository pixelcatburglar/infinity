use super::*;

fn row(key: &str, data: &str) -> dao::SkillPlanRow {
    dao::SkillPlanRow { key: Some(key.into()), data: data.into() }
}

#[test]
fn preserves_historical_intermediate_and_zero_level_edges() {
    // The middle type represents an unpublished non-skill; only existence matters.
    let known = [101, 102, 103];
    let graph = SkillPlans::from_rows(
        vec![row("1", r#"{"_key":1}"#)],
        vec![
            row("101", r#"{"_key":101,"102":3,"103":0}"#),
            row("102", r#"{"_key":102,"103":5}"#),
            row("103", r#"{"_key":103}"#),
        ],
        |id| known.contains(&id),
    ).unwrap();
    assert!(graph.contains(1));
    assert_eq!(graph.requirements(101), &[(102, 3), (103, 0)]);
    assert_eq!(graph.requirements(102), &[(103, 5)]);
    assert!(graph.requirements(103).is_empty());
}

#[test]
fn rejects_invalid_prerequisite_rows() {
    for (key, data) in [
        ("101", "not-json"),
        ("101", r#"{"_key":102}"#),
        ("0", r#"{"_key":0}"#),
        ("101", r#"{"_key":101,"0":1}"#),
        ("101", r#"{"_key":101,"999":1}"#),
        ("101", r#"{"_key":101,"102":-1}"#),
        ("101", r#"{"_key":101,"102":6}"#),
        ("101", r#"{"_key":101,"102":1.5}"#),
    ] {
        assert!(SkillPlans::from_rows(vec![], vec![row(key, data)], |id| [101, 102].contains(&id)).is_err(), "{data}");
    }
    assert!(SkillPlans::from_rows(vec![], vec![row("101", r#"{"_key":101}"#), row("101", r#"{"_key":101}"#)], |_| true).is_err());
}

#[test]
fn cyclic_graphs_preserve_edges_and_mark_ancestors() {
    let rows = vec![
        row("101", r#"{"_key":101,"101":5,"102":1}"#),
        row("102", r#"{"_key":102}"#),
        row("103", r#"{"_key":103,"104":1}"#),
        row("104", r#"{"_key":104,"103":2}"#),
        row("105", r#"{"_key":105,"101":1,"103":1}"#),
        row("106", r#"{"_key":106,"102":1}"#),
    ];
    let graph = SkillPlans::from_rows(vec![], rows, |_| true).unwrap();
    assert_eq!(graph.requirements(101), &[(101, 5), (102, 1)]);
    assert_eq!(graph.requirements(103), &[(104, 1)]);
    assert_eq!(graph.requirements(104), &[(103, 2)]);
    for id in [101, 103, 104, 105] { assert!(graph.is_cyclic(id)); }
    for id in [102, 106, 107] { assert!(!graph.is_cyclic(id)); }
}
