use super::*;

#[test]
fn client_write_timeout_defaults_custom_value_and_rejects_zero() {
    let fixture = Fixture::new();
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().server.client_write_timeout_secs, 30);
    fixture.write("[server]\nclient_write_timeout_secs = 5");
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().server.client_write_timeout_secs, 5);
    assert!(load_error("[server]\nclient_write_timeout_secs = 0").contains("server.client_write_timeout_secs"));
}
