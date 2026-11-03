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
    #[prost(bytes = "vec", optional, tag = "1")]
    pub application_instance_uuid: Option<Vec<u8>>,
    #[prost(uint32, optional, tag = "2")]
    pub solar_system: Option<u32>,
    #[prost(sint32, optional, tag = "3")]
    pub user: Option<i32>,
    #[prost(uint32, optional, tag = "4")]
    pub character: Option<u32>,
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

pub fn now_timestamp() -> prost_types::Timestamp {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    prost_types::Timestamp { seconds: d.as_secs() as i64, nanos: d.subsec_nanos() as i32 }
}
