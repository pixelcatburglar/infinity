use super::*;
use crate::gateway::message::{msg_field, str_field};
use prost::Message;
use prost_reflect::{DynamicMessage, ReflectMessage};
use prost_reflect::Value;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn checkpoint_uses_loaded_catalogue_metadata() {
    let gateway = crate::gateway::tests::gateway().await;
    let ctx = HandlerContext::for_request(&gateway, crate::gateway::Caller::default()).await.unwrap();
    let request = "eve_public.achievement.definition.api.cdn.GetLatestRequest";
    let response = handle(&ctx, request).unwrap();
    let descriptor = crate::gateway::message::message(request).unwrap();
    let mut with_version = DynamicMessage::new(descriptor.clone());
    let mut current = crate::gateway::message::child(&descriptor, "current_version").unwrap();
    crate::gateway::message::set(&mut current, "major", Value::U32(1)).unwrap();
    crate::gateway::message::set(&mut with_version, "current_version", Value::Message(current)).unwrap();
    let mut without_version = DynamicMessage::new(descriptor);
    crate::gateway::message::set(&mut without_version, "no_local_version_available", Value::Bool(true)).unwrap();
    let first = handle(&ctx, with_version.descriptor().full_name()).unwrap();
    let second = handle(&ctx, without_version.descriptor().full_name()).unwrap();
    assert_eq!(first.encode_to_vec(), second.encode_to_vec());
    let cp = msg_field(&response, "checkpoint").unwrap();
    assert_eq!(str_field(&cp, "url"), format!("{}/content/achievements", gateway.config.cdn_url()));
    let version = msg_field(&cp, "version").unwrap();
    assert_eq!(version.get_field_by_name("major").unwrap().as_ref(), &Value::U32(1));
    assert_eq!(version.get_field_by_name("minor").unwrap().as_ref(), &Value::U32(0));
    assert_eq!(version.get_field_by_name("patch").unwrap().as_ref(), &Value::U32(7));
    assert_eq!(version.get_field_by_name("prerelease_tags").unwrap().as_ref(), &Value::List(Vec::new()));
    assert_eq!(version.get_field_by_name("build_tags").unwrap().as_ref(), &Value::List(Vec::new()));
    let crc = msg_field(&cp, "crc").unwrap();
    let bytes = crate::cdn::content("achievements").unwrap().0;
    assert_eq!(crc.get_field_by_name("crc").unwrap().as_ref(), &Value::U32(crate::cdn::crc32(bytes)));
}
