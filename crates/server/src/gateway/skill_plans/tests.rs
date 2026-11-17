//! Shared fixture for skill-plan gateway tests: corporation rows, role grants,
//! request builders and refusal assertions reused by the corporation plan and
//! milestone adapters. Personal adapter tests keep their own helpers.
use bytes::Bytes;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::gateway::{Caller, Gateway, context::HandlerContext, error::{FailureCode, GatewayError}, message::{child, identifier, message, set}};

/// Client appConst.py:372 corporation skill-plan manager bit, asserted through
/// real role columns the same way the client grants it.
pub(super) const MANAGER_ROLE: i64 = 1 << 62;

pub(super) fn request(name: &str) -> DynamicMessage {
    DynamicMessage::new(message(name).unwrap())
}

pub(super) fn uuid_request(name: &str, id: &[u8]) -> DynamicMessage {
    let mut req = request(name);
    let mut plan = child(&req.descriptor(), "skill_plan").unwrap();
    set(&mut plan, "uuid", PV::Bytes(Bytes::from(id.to_vec()))).unwrap();
    set(&mut req, "skill_plan", PV::Message(plan)).unwrap();
    req
}

pub(super) fn category_request(name: &str, id: &[u8], category: &[u8]) -> DynamicMessage {
    let mut req = uuid_request(name, id);
    let mut value = child(&req.descriptor(), "category").unwrap();
    set(&mut value, "uuid", PV::Bytes(Bytes::from(category.to_vec()))).unwrap();
    set(&mut req, "category", PV::Message(value)).unwrap();
    req
}

pub(super) fn set_requirements(req: &mut DynamicMessage, field: &str, values: &[(u64, u32)]) {
    let mut entries = Vec::new();
    for &(skill, level) in values {
        let mut item = child(&req.descriptor(), field).unwrap();
        let skill_type = identifier(&item.descriptor(), "skill_type", PV::U64(skill)).unwrap();
        set(&mut item, "skill_type", PV::Message(skill_type)).unwrap();
        set(&mut item, "level", PV::U32(level)).unwrap();
        entries.push(PV::Message(item));
    }
    set(req, field, PV::List(entries)).unwrap();
}
/// A fresh 16-byte UUID the same way `new_uuid` shapes message UUIDs.
pub(super) fn uuid_bytes() -> Vec<u8> { [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat() }

pub(super) fn response_uuid(response: &DynamicMessage, field: &str) -> Vec<u8> {
    response.get_field_by_name(field).unwrap().as_message().unwrap()
        .get_field_by_name("uuid").unwrap().as_bytes().unwrap().to_vec()
}

pub(super) fn refused(error: GatewayError, code: FailureCode) {
    assert!(matches!(error, GatewayError::Refused { code: actual, .. } if std::mem::discriminant(&actual) == std::mem::discriminant(&code)));
}

pub(super) async fn context<'a>(gw: &'a Gateway, account: i64, character: i64) -> HandlerContext<'a> {
    let mut ctx = HandlerContext::for_request(gw, Caller { user_id: Some(account), character_id: Some(character) }).await.unwrap();
    ctx.application_instance_uuid = Some([8; 16]);
    ctx
}

/// A live player corporation row with `ceo` as creator; members join through `assign`.
pub(super) async fn corporation(db: &crate::db::Db, ceo: i64) -> i64 {
    db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('skill-plan-' || nextval('game.corporations_corporation_id_seq')::text, 'SP' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&ceo]).await.unwrap().get("corporation_id")
}

/// Point `character` at `corporation` with the given global/grantable/title role bits.
pub(super) async fn assign(db: &crate::db::Db, character: i64, corporation: i64, roles: i64, grantable: i64, title_mask: i64, blocked: bool) {
    db.execute("UPDATE game.characters SET corporation_id=$2, corp_roles=$3, corp_roles_hq=0, corp_roles_base=0, corp_roles_other=0, corp_grantable_roles=$4, corp_grantable_hq=0, corp_grantable_base=0, corp_grantable_other=0, corp_title_mask=$5, corp_block_roles=$6 WHERE character_id=$1", &[&character, &corporation, &roles, &grantable, &title_mask, &blocked]).await.unwrap();
}
