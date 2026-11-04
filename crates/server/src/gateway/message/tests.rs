use super::*;

#[test]
fn response_name_requires_request_suffix() {
    assert_eq!(response_name("eve_public.ExampleRequest").as_deref(), Some("eve_public.ExampleResponse"));
    assert_eq!(response_name("eve_public.Example"), None);
}

#[test]
fn descriptor_pool_resolves() {
    assert!(message("eve_public.Request").is_some());
    assert!(message("google.protobuf.Any").is_some());
    let svc = pool().get_service_by_name("eve_public.gateway.Requests").expect("Requests service");
    assert!(svc.methods().any(|m| m.name() == "Send"));
}
