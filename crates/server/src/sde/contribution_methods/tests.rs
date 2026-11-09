use super::*;

fn row(key: &str, json: &str) -> dao::Row {
    dao::Row { key: Some(key.to_owned()), data: json.to_owned() }
}

#[test]
fn native_metadata_is_parsed_and_identity_checked() {
    let json = r#"{"_key":"Example","titleID":1,"descriptionID":2,"contentTags":["ignored"],"parameters":[{"parameterKey":"item","parameterType":"type","isRequired":1}]}"#;
    let methods = ContributionMethods::from_rows(vec![row("Example", json)]).unwrap();
    let method = methods.get("Example").unwrap();
    assert_eq!(method.parameters[0].is_required, 1);
    assert!(ContributionMethods::from_rows(vec![row("Other", json)]).is_err());
    assert!(ContributionMethods::from_rows(vec![row("Example", json), row("Example", json)]).is_err());
}
