use super::{GatewayNotice, NoticeHub};
use crate::{gateway::{message::message, protocol::{Notice, target_group::Target}}, session::OnlineSession};
use prost::Message;
use prost_reflect::DynamicMessage;

fn session() -> OnlineSession {
    OnlineSession { character_id: 7, user_id: Some(-8), session_id: 1, ship_id: None, station_id: None, system_id: Some(9), corporation_id: Some(10), alliance_id: Some(-11), faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }
}
fn payload() -> DynamicMessage { DynamicMessage::new(message("eve_public.Event").unwrap()) }
fn notice(target: Target) -> GatewayNotice { GatewayNotice::new(target, &payload()).unwrap() }
fn decode(frame: &[u8]) -> Notice { Notice::decode(&frame[5..]).unwrap() }

#[tokio::test]
async fn targets_match_only_current_identity_and_fanout_has_one_tenant() {
    let hub = NoticeHub::new();
    let app = [3; 16];
    let lease = hub.lease(app);
    lease.bind(&session(), |_| Some(session()));
    let mut registration = hub.register(app, 8);
    for target in [Target::ApplicationInstanceUuid(app.to_vec()), Target::SolarSystem(9), Target::User(-8), Target::Character(7), Target::Corporation(10), Target::Alliance(-11)] {
        hub.dispatch(notice(target.clone()), |_| Some(session()));
        let frame = registration.rx.try_recv().unwrap();
        let envelope = decode(&frame);
        assert_eq!(envelope.tenant, super::DEPLOYMENT_TENANT);
        assert_eq!(envelope.target_group.unwrap().target, Some(target));
    }
    for target in [Target::ApplicationInstanceUuid(vec![4;16]), Target::SolarSystem(8), Target::User(-9), Target::Character(8), Target::Corporation(9), Target::Alliance(-12)] {
        hub.dispatch(notice(target), |_| Some(session()));
        assert!(registration.rx.try_recv().is_err());
    }
    hub.dispatch(notice(Target::Character(7)), |_| None);
    assert!(registration.rx.try_recv().is_err());
}

#[test]
fn target_oneof_boundaries_and_retarget() {
    let payload = payload();
    let targets = [
        Target::User(i32::MIN), Target::User(i32::MAX),
        Target::Alliance(i32::MIN), Target::Alliance(i32::MAX),
        Target::Character(0), Target::Character(u32::MAX),
        Target::Corporation(0), Target::Corporation(u32::MAX),
        Target::SolarSystem(0), Target::SolarSystem(u32::MAX),
        Target::ApplicationInstanceUuid(vec![7; 16]),
    ];
    let first = GatewayNotice::new(targets[0].clone(), &payload).unwrap();
    let original = decode(&first.frame());
    assert_eq!(original.uuid.len(), 16);
    for target in targets {
        let framed = decode(&first.retarget(target.clone()).unwrap().frame());
        assert_eq!(framed.target_group.unwrap().target, Some(target));
        assert_eq!(framed.uuid, original.uuid);
        assert_eq!(framed.payload, original.payload);
    }
    for length in [0, 15, 17] {
        let invalid = Target::ApplicationInstanceUuid(vec![0; length]);
        assert!(GatewayNotice::new(invalid.clone(), &payload).is_err());
        assert!(first.retarget(invalid).is_err());
    }
}

#[test]
fn foreign_descriptor_with_known_name_is_rejected() {
    let file = prost_types::FileDescriptorProto {
        name: Some("foreign.proto".into()),
        package: Some("eve_public".into()),
        syntax: Some("proto3".into()),
        message_type: vec![prost_types::DescriptorProto {
            name: Some("Event".into()),
            field: vec![prost_types::FieldDescriptorProto {
                name: Some("different".into()),
                number: Some(1),
                label: Some(prost_types::field_descriptor_proto::Label::Optional as i32),
                r#type: Some(prost_types::field_descriptor_proto::Type::String as i32),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let set = prost_types::FileDescriptorSet { file: vec![file] };
    let pool = prost_reflect::DescriptorPool::decode(set.encode_to_vec().as_slice()).unwrap();
    let foreign = DynamicMessage::new(pool.get_message_by_name("eve_public.Event").unwrap());
    assert!(GatewayNotice::new(Target::Character(7), &foreign).is_err());
}

#[tokio::test]
async fn replacement_and_full_cancel_only_the_matching_writer() {
    let hub = NoticeHub::new();
    let app = [4;16];
    let old_lease = hub.lease(app);
    old_lease.bind(&session(), |_| Some(session()));
    let mut old = hub.register(app, 1);
    hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
    let mut replacement = hub.register(app, 1);
    assert!(old.cancelled.try_recv().is_ok());
    old_lease.bind(&session(), |_| Some(session()));
    drop(old);
    hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
    assert!(replacement.rx.try_recv().is_ok());
    hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
    hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
    assert!(replacement.cancelled.try_recv().is_ok());
}

#[tokio::test]
async fn current_groups_and_session_generation_are_authoritative() {
    let hub = NoticeHub::new();
    let app = [5;16];
    let lease = hub.lease(app);
    lease.bind(&session(), |_| Some(session()));
    let mut registration = hub.register(app, 4);
    let changed = OnlineSession { system_id: Some(12), corporation_id: Some(13), ship_id: Some(14), ..session() };
    hub.dispatch(notice(Target::SolarSystem(9)), |_| Some(changed.clone()));
    assert!(registration.rx.try_recv().is_err());
    hub.dispatch(notice(Target::Corporation(13)), |_| Some(changed.clone()));
    assert!(registration.rx.try_recv().is_ok());
    let replaced = OnlineSession { session_id: 2, ..changed };
    hub.dispatch(notice(Target::Character(7)), |_| Some(replaced.clone()));
    assert!(registration.rx.try_recv().is_err());
    hub.dispatch(notice(Target::Character(7)), |_| Some(replaced.clone()));
    assert!(registration.rx.try_recv().is_err());
}


#[tokio::test]
async fn absent_fields_and_unbound_stream_never_match() {
    let hub = NoticeHub::new();
    let app = [9;16];
    let mut stream = hub.register(app, 8);
    hub.dispatch(notice(Target::ApplicationInstanceUuid(app.to_vec())), |_| None);
    assert!(stream.rx.try_recv().is_ok());
    hub.dispatch(notice(Target::ApplicationInstanceUuid([8;16].to_vec())), |_| None);
    assert!(stream.rx.try_recv().is_err());
    for target in [Target::User(-8), Target::Character(7), Target::Corporation(10), Target::Alliance(-11), Target::SolarSystem(9)] {
        hub.dispatch(notice(target), |_| Some(session()));
        assert!(stream.rx.try_recv().is_err());
    }
    let lease = hub.lease(app);
    lease.bind(&session(), |_| Some(session()));
    let missing = OnlineSession { system_id: None, corporation_id: None, alliance_id: None, ..session() };
    for target in [Target::SolarSystem(9), Target::Corporation(10), Target::Alliance(-11)] {
        hub.dispatch(notice(target), |_| Some(missing.clone()));
        assert!(stream.rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn full_queue_does_not_block_other_streams() {
    let hub = NoticeHub::new();
    let first = [1;16];
    let second = [2;16];
    let a = hub.lease(first);
    let b = hub.lease(second);
    a.bind(&session(), |_| Some(session()));
    b.bind(&session(), |_| Some(session()));
    let mut slow = hub.register(first, 1);
    let mut fast = hub.register(second, 2);
    hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
    hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
    assert!(slow.cancelled.try_recv().is_ok());
    assert!(fast.rx.try_recv().is_ok());
    assert!(fast.rx.try_recv().is_ok());
}

#[tokio::test]
async fn either_open_order_uses_the_same_live_binding_and_last_lease_releases_it() {
    for request_first in [true, false] {
        let hub = NoticeHub::new();
        let app = [6;16];
        let (lease, mut stream) = if request_first {
            let lease = hub.lease(app);
            lease.bind(&session(), |_| Some(session()));
            (lease, hub.register(app, 1))
        } else {
            let stream = hub.register(app, 1);
            let lease = hub.lease(app);
            lease.bind(&session(), |_| Some(session()));
            (lease, stream)
        };
        hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
        assert!(stream.rx.try_recv().is_ok());
        drop(lease);
        drop(stream);
        let mut fresh = hub.register(app, 1);
        hub.dispatch(notice(Target::Character(7)), |_| Some(session()));
        assert!(fresh.rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn cancelled_old_writer_cannot_remove_replacement() {
    let hub = NoticeHub::new();
    let app = [12; 16];
    let mut old = hub.register(app, 1);
    hub.dispatch(notice(Target::ApplicationInstanceUuid(app.to_vec())), |_| None);
    hub.dispatch(notice(Target::ApplicationInstanceUuid(app.to_vec())), |_| None);
    assert!(old.cancelled.try_recv().is_ok());
    let mut current = hub.register(app, 1);
    drop(old);
    hub.dispatch(notice(Target::ApplicationInstanceUuid(app.to_vec())), |_| None);
    assert!(current.rx.try_recv().is_ok());
}

#[tokio::test]
async fn logout_and_stale_binding_do_not_reach_replacement_session() {
    let hub = NoticeHub::new();
    let app = [7;16];
    let lease = hub.lease(app);
    lease.bind(&session(), |_| Some(session()));
    let mut stream = hub.register(app, 4);
    hub.dispatch(notice(Target::Character(7)), |_| None);
    assert!(stream.rx.try_recv().is_err());
    lease.bind(&session(), |_| Some(session()));
    let replacement = OnlineSession { session_id: 2, ..session() };
    hub.dispatch(notice(Target::Character(7)), |_| Some(replacement.clone()));
    assert!(stream.rx.try_recv().is_err());
    lease.bind(&replacement, |_| Some(replacement.clone()));
    hub.dispatch(notice(Target::Character(7)), |_| Some(replacement.clone()));
    assert!(stream.rx.try_recv().is_ok());
}

#[tokio::test]
async fn current_session_rebinds_immediately_and_rejects_stale_or_live_conflicts() {
    let hub = NoticeHub::new();
    let app = [13; 16];
    let lease = hub.lease(app);
    let old = session();
    lease.bind(&old, |id| (id == old.character_id).then(|| old.clone()));
    let mut stream = hub.register(app, 4);
    let current = OnlineSession { session_id: 2, corporation_id: Some(21), system_id: Some(22), ..old.clone() };
    let resolve = |id| (id == current.character_id).then(|| current.clone());
    lease.bind(&current, resolve);
    for target in [Target::Character(7), Target::Corporation(21), Target::SolarSystem(22)] {
        hub.dispatch(notice(target), resolve);
        assert!(stream.rx.try_recv().is_ok());
    }
    lease.bind(&old, resolve);
    let stale_account = OnlineSession { user_id: Some(-99), ..current.clone() };
    lease.bind(&stale_account, resolve);
    let conflict = OnlineSession { character_id: 8, user_id: Some(-9), session_id: 3, ..current.clone() };
    let both = |id| if id == current.character_id { Some(current.clone()) } else if id == conflict.character_id { Some(conflict.clone()) } else { None };
    lease.bind(&conflict, both);
    hub.dispatch(notice(Target::Character(7)), both);
    assert!(stream.rx.try_recv().is_ok());
    hub.dispatch(notice(Target::Character(8)), both);
    assert!(stream.rx.try_recv().is_err());
}

#[tokio::test]
async fn bind_compare_and_swap_preserves_concurrent_replacement() {
    let hub = NoticeHub::new();
    let app = [14; 16];
    let lease = hub.lease(app);
    let old = session();
    lease.bind(&old, |_| Some(old.clone()));
    let candidate = OnlineSession { character_id: 8, session_id: 2, ..old.clone() };
    let winner = OnlineSession { character_id: 9, session_id: 3, ..old.clone() };
    let invoked = std::cell::Cell::new(false);
    lease.bind(&candidate, |id| {
        if !invoked.replace(true) {
            lease.bind(&winner, |id| (id == winner.character_id).then(|| winner.clone()));
        }
        (id == candidate.character_id).then(|| candidate.clone())
    });
    let mut stream = hub.register(app, 1);
    hub.dispatch(notice(Target::Character(9)), |id| (id == winner.character_id).then(|| winner.clone()));
    assert!(stream.rx.try_recv().is_ok());
}