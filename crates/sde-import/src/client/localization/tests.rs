use super::*;
use serde_json::json;

#[test]
fn main_retains_tagged_graph_and_native_rows() {
    let raw = b"}(S'labels'\n}(S'category'\nS'value'\nuS'languages'\n]S'en'\naS'types'\n}S'important'\n]u.";
    let decoded = decode("localization_fsd_main", raw).unwrap();
    assert_eq!(decoded.records, vec![json!({"_key":"labels","text":{"category":"value"}}), json!({"_key":"languages","text":["en"]}), json!({"_key":"types","text":{}}), json!({"_key":"important","text":[]})]);
    assert_eq!(decoded.metadata, json!({"$dict":[["labels",{"$dict":[["category","value"]]}],["languages",["en"]],["types",{"$dict":[]}],["important",[]]]}));
}

#[test]
fn language_projects_tuple_without_fabricated_language() {
    let raw = b"(S'en'\n}K\x01(V\\u4e2d\n}(S'kind'\nS'label'\nu](S'token'\nU\x02oketst.";
    let decoded = decode("localization_fsd_en_us", raw).unwrap();
    assert_eq!(decoded.records, vec![json!({"_key":1,"text":"中","metadata":{"kind":"label"},"tokens":["token","ok"]})]);
    assert_eq!(decoded.metadata, json!({"$tuple":["en",{"$dict":[[1,{"$tuple":["中",{"$dict":[["kind","label"]]},["token","ok"]]}]]}]}));
    assert!(decoded.records[0].get("languageID").is_none());
}

#[test]
fn rejects_bad_message_shapes_and_colliding_projection_keys() {
    assert!(decode("localization_fsd_zh", b"(NX\x01\x00\x00\x00xt.").is_err());
    assert!(decode("localization_fsd_zh", b"(N}K\x01Nst.").is_err());
    assert!(decode("unknown", b"N.").is_err());
    assert!(decode("localization_fsd_main", b"}(X\x01\x00\x00\x00x}(K\x01NX\x01\x00\x00\x001Nuu.").is_err());
}
