use super::*;

#[test]
fn embedded_descriptors_validate() {
    validate().expect("embedded gateway descriptors must satisfy startup validation");
}

pub(super) async fn gateway() -> Arc<Gateway> {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    Gateway::new(db, world, Arc::new(Config::default()))
}

pub(crate) fn notice_stream(world: &World, application: [u8; 16], character: i64, capacity: usize) -> notices::registry::Registration {
    let hub = world.notice_hub();
    let lease = hub.lease(application);
    let session = world.online_session(character).expect("online character");
    lease.bind(&session, |id| world.online_session(id));
    hub.register(application, capacity)
}

pub(crate) fn decode_notice(frame: &[u8]) -> (protocol::Notice, prost_reflect::DynamicMessage) {
    use prost::Message;
    let notice = protocol::Notice::decode(&frame[5..]).unwrap();
    let payload = notice.payload.as_ref().unwrap();
    let descriptor = message::message(message::type_name(&payload.type_url)).unwrap();
    let decoded = prost_reflect::DynamicMessage::decode(descriptor, payload.value.as_slice()).unwrap();
    (notice, decoded)
}
