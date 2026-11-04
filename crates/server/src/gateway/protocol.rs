//! Envelope types and timestamps for `requestBroker.py:new_eve_public_request`.
//! Uses protobuf fields and the system clock, not World or DAO data; these types
//! do not implement event processing or repair notice delivery.
use prost::Message;

#[derive(Clone, PartialEq, Message)]
pub struct Identifier32 {
    #[prost(uint32, tag = "1")]
    pub sequential: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct UserIdentifier {
    #[prost(sint32, tag = "1")]
    pub sequential: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct ActiveIdentity {
    #[prost(message, optional, tag = "1")]
    pub character: Option<Identifier32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AuthoritativeContext {
    #[prost(string, tag = "3")]
    pub tenant: String,
    #[prost(message, optional, tag = "5")]
    pub authenticated_user: Option<UserIdentifier>,
    #[prost(message, optional, tag = "7")]
    pub active_character: Option<Identifier32>,
    #[prost(message, optional, tag = "10")]
    pub identity: Option<ActiveIdentity>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Request {
    #[prost(message, optional, tag = "1")]
    pub issued: Option<prost_types::Timestamp>,
    #[prost(bytes = "vec", tag = "2")]
    pub correlation_uuid: Vec<u8>,
    #[prost(message, optional, tag = "3")]
    pub payload: Option<prost_types::Any>,
    #[prost(string, tag = "4")]
    pub external_origin: String,
    #[prost(bytes = "vec", tag = "5")]
    pub application_instance_uuid: Vec<u8>,
    #[prost(message, optional, tag = "6")]
    pub authoritative_context: Option<AuthoritativeContext>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Response {
    #[prost(message, optional, tag = "1")]
    pub dispatched: Option<prost_types::Timestamp>,
    #[prost(bytes = "vec", tag = "2")]
    pub correlation_uuid: Vec<u8>,
    #[prost(uint32, tag = "3")]
    pub status_code: u32,
    #[prost(string, tag = "4")]
    pub status_message: String,
    #[prost(message, optional, tag = "5")]
    pub payload: Option<prost_types::Any>,
    #[prost(string, tag = "6")]
    pub internal_origin: String,
    #[prost(bytes = "vec", tag = "7")]
    pub application_instance_uuid: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TargetGroup {
    #[prost(oneof = "target_group::Target", tags = "1, 2, 3, 4, 5, 6")]
    pub target: Option<target_group::Target>,
}

pub mod target_group {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Target {
        #[prost(bytes, tag = "1")]
        ApplicationInstanceUuid(Vec<u8>),
        #[prost(uint32, tag = "2")]
        SolarSystem(u32),
        #[prost(sint32, tag = "3")]
        User(i32),
        #[prost(uint32, tag = "4")]
        Character(u32),
        #[prost(uint32, tag = "5")]
        Corporation(u32),
        #[prost(int32, tag = "6")]
        Alliance(i32),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Notice {
    #[prost(message, optional, tag = "1")]
    pub dispatched: Option<prost_types::Timestamp>,
    #[prost(bytes = "vec", tag = "2")]
    pub uuid: Vec<u8>,
    #[prost(string, tag = "3")]
    pub internal_origin: String,
    #[prost(string, tag = "4")]
    pub tenant: String,
    #[prost(message, optional, tag = "5")]
    pub payload: Option<prost_types::Any>,
    #[prost(message, optional, tag = "6")]
    pub target_group: Option<TargetGroup>,
}

impl Notice {
    /// Encode the already-serialized Any without copying its value into another message.
    pub(super) fn encode_with_payload(&self, payload: &[u8], buf: &mut impl bytes::BufMut) {
        self.encode_raw(buf);
        prost::encoding::encode_key(5, prost::encoding::WireType::LengthDelimited, buf);
        prost::encoding::encode_varint(payload.len() as u64, buf);
        buf.put_slice(payload);
    }

    pub(super) fn encoded_len_with_payload(&self, payload: &[u8]) -> usize {
        self.encoded_len() + 1 + prost::encoding::encoded_len_varint(payload.len() as u64) + payload.len()
    }
}

pub fn now_timestamp() -> prost_types::Timestamp {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    prost_types::Timestamp { seconds: d.as_secs() as i64, nanos: d.subsec_nanos() as i32 }
}
