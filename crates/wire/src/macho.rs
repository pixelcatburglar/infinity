//! machoNet packets (carbon/common/script/net/machoNetPacket.py) and
//! addresses (machoNetAddress.py). Both are old-style classes marshalled as
//! `TY_INSTANCE(guid, __getstate__())`.

use anyhow::{anyhow, bail, Result};
use marshal::Value;

/// Client carbon/common/script/net/machoNetPacket.py command identifiers.
pub mod cmd {
    /// Client machoNetPacket IdentificationReq.
    pub const IDENTIFICATION_REQ: i64 = 2;
    /// Client machoNetPacket IdentificationRsp.
    pub const IDENTIFICATION_RSP: i64 = 3;
    /// Client machoNetPacket CallReq.
    pub const CALL_REQ: i64 = 6;
    /// Client machoNetPacket CallRsp.
    pub const CALL_RSP: i64 = 7;
    /// Client machoNetPacket TransportClosed.
    pub const TRANSPORTCLOSED: i64 = 8;
    /// Client machoNetPacket Notification.
    pub const NOTIFICATION: i64 = 12;
    /// Client machoNetPacket ErrorResponse.
    pub const ERRORRESPONSE: i64 = 15;
    /// Client machoNetPacket SessionChangeNotification.
    pub const SESSIONCHANGENOTIFICATION: i64 = 16;
    /// Client machoNetPacket SessionInitialStateNotification.
    pub const SESSIONINITIALSTATENOTIFICATION: i64 = 18;
    /// Client machoNetPacket PingReq.
    pub const PING_REQ: i64 = 20;
    /// Client machoNetPacket PingRsp.
    pub const PING_RSP: i64 = 21;
}

/// Client machoNet MACHONETERR_WRAPPEDEXCEPTION.
pub const ERR_WRAPPEDEXCEPTION: i64 = 2;
/// Client machoNetAddress ADDRESS_TYPE_NODE.
const ADDRESS_NODE: i64 = 1;
/// Client machoNetAddress ADDRESS_TYPE_CLIENT.
const ADDRESS_CLIENT: i64 = 2;
/// Client machoNetAddress ADDRESS_TYPE_BROADCAST.
const ADDRESS_BROADCAST: i64 = 4;
/// Client machoNetAddress ADDRESS_TYPE_ANY.
const ADDRESS_ANY: i64 = 8;
/// Client MachoPacket.__getstate__: tracing fields after the six core fields.
const PACKET_TAIL_FIELDS: usize = 8;
/// Client MachoPacket.__getstate__: number of mandatory core fields.
const PACKET_CORE_FIELDS: usize = 6;

fn class_for(command: i64) -> &'static str {
    match command {
        cmd::IDENTIFICATION_REQ => "carbon.common.script.net.machoNetPacket.IdentificationReq",
        cmd::IDENTIFICATION_RSP => "carbon.common.script.net.machoNetPacket.IdentificationRsp",
        cmd::CALL_REQ => "carbon.common.script.net.machoNetPacket.CallReq",
        cmd::CALL_RSP => "carbon.common.script.net.machoNetPacket.CallRsp",
        cmd::TRANSPORTCLOSED => "carbon.common.script.net.machoNetPacket.TransportClosed",
        cmd::NOTIFICATION => "carbon.common.script.net.machoNetPacket.Notification",
        cmd::ERRORRESPONSE => "carbon.common.script.net.machoNetPacket.ErrorResponse",
        cmd::SESSIONCHANGENOTIFICATION => "carbon.common.script.net.machoNetPacket.SessionChangeNotification",
        cmd::SESSIONINITIALSTATENOTIFICATION => "carbon.common.script.net.machoNetPacket.SessionInitialStateNotification",
        cmd::PING_REQ => "carbon.common.script.net.machoNetPacket.PingReq",
        cmd::PING_RSP => "carbon.common.script.net.machoNetPacket.PingRsp",
        _ => "carbon.common.script.net.machoNetPacket.MachoPacket",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Address {
    /// ADDRESS_TYPE_NODE = 1
    Node { node_id: i64, service: Option<String>, call_id: Value },
    /// ADDRESS_TYPE_CLIENT = 2
    Client { client_id: i64, call_id: Value, service: Option<String> },
    /// ADDRESS_TYPE_BROADCAST = 4
    Broadcast { broadcast_id: Value, narrowcast: Value, idtype: Value },
    /// ADDRESS_TYPE_ANY = 8
    Any { service: Option<String>, call_id: Value },
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_text().map(|s| s.into_owned())
}

fn str_or_none(s: &Option<String>) -> Value {
    s.as_ref().map(Value::str).unwrap_or(Value::None)
}

impl Address {
    pub fn service(&self) -> Option<&str> {
        match self {
            Address::Node { service, .. } | Address::Client { service, .. } | Address::Any { service, .. } => service.as_deref(),
            Address::Broadcast { .. } => None,
        }
    }

    pub fn from_value(v: &Value) -> Result<Address> {
        let Value::Instance(_, state) = v else { bail!("address is not an instance: {v:?}") };
        let s = state.items().ok_or_else(|| anyhow!("address state is not a tuple"))?;
        let get = |i: usize| s.get(i).cloned().unwrap_or(Value::None);
        Ok(match get(0).as_int() {
            Some(ADDRESS_NODE) => Address::Node { node_id: get(1).as_int().unwrap_or(0), service: opt_str(&get(2)), call_id: get(3) },
            Some(ADDRESS_CLIENT) => Address::Client { client_id: get(1).as_int().unwrap_or(0), call_id: get(2), service: opt_str(&get(3)) },
            Some(ADDRESS_BROADCAST) => Address::Broadcast { broadcast_id: get(1), narrowcast: get(2), idtype: get(3) },
            Some(ADDRESS_ANY) => Address::Any { service: opt_str(&get(1)), call_id: get(2) },
            other => bail!("unknown address type {other:?}"),
        })
    }

    pub fn to_value(&self) -> Value {
        let state = match self {
            Address::Node { node_id, service, call_id } => {
                vec![Value::Int(ADDRESS_NODE), Value::Int(*node_id), str_or_none(service), call_id.clone()]
            }
            Address::Client { client_id, call_id, service } => {
                vec![Value::Int(ADDRESS_CLIENT), Value::Int(*client_id), call_id.clone(), str_or_none(service)]
            }
            Address::Broadcast { broadcast_id, narrowcast, idtype } => {
                vec![Value::Int(ADDRESS_BROADCAST), broadcast_id.clone(), narrowcast.clone(), idtype.clone()]
            }
            Address::Any { service, call_id } => vec![Value::Int(ADDRESS_ANY), str_or_none(service), call_id.clone()],
        };
        Value::Instance(Box::new(Value::str("carbon.common.script.net.machoNetAddress.MachoAddress")), Box::new(Value::Tuple(state)))
    }
}

/// `MachoPacket.__getstate__` 14-tuple.
#[derive(Debug, Clone)]
pub struct Packet {
    pub command: i64,
    pub source: Address,
    pub destination: Address,
    pub user_id: Value,
    pub body: Vec<Value>,
    pub oob: Value,
    /// contextKey, journeyID, trace_id, parent_span_id, sampled, ingress_id, sample_rate, cut_parent
    pub tail: Vec<Value>,
    /// Local marker for scene-bound deliveries dropped on ballpark reset; never encoded.
    pub scene_bound: bool,
}

impl Packet {
    pub fn new(command: i64, source: Address, destination: Address, user_id: Value, body: Vec<Value>) -> Packet {
        Packet { command, source, destination, user_id, body, oob: Value::None, tail: vec![Value::None; PACKET_TAIL_FIELDS], scene_bound: false }
    }

    pub fn from_value(v: &Value) -> Result<Packet> {
        let Value::Instance(_, state) = v else { bail!("packet is not an instance") };
        let s = state.items().ok_or_else(|| anyhow!("packet state is not a tuple"))?;
        if s.len() < PACKET_CORE_FIELDS {
            bail!("packet state too short ({})", s.len());
        }
        let mut tail: Vec<Value> = s[PACKET_CORE_FIELDS..].to_vec();
        tail.resize(PACKET_TAIL_FIELDS, Value::None);
        Ok(Packet {
            command: s[0].as_int().ok_or_else(|| anyhow!("packet command is not an int"))?,
            source: Address::from_value(&s[1])?,
            destination: Address::from_value(&s[2])?,
            user_id: s[3].clone(),
            body: s[4].items().map(|b| b.to_vec()).unwrap_or_default(),
            oob: s[5].clone(),
            tail, scene_bound: false,
        })
    }

    pub fn to_value(&self) -> Value {
        let mut state = vec![
            Value::Int(self.command),
            self.source.to_value(),
            self.destination.to_value(),
            self.user_id.clone(),
            Value::Tuple(self.body.clone()),
            self.oob.clone(),
        ];
        state.extend(self.tail.iter().cloned());
        Value::Instance(Box::new(Value::str(class_for(self.command))), Box::new(Value::Tuple(state)))
    }

    /// `MachoPacket.Response`: swap addresses, keep user and tracing fields.
    pub fn response(&self, command: i64, body: Vec<Value>) -> Packet {
        let mut tail = vec![Value::None; PACKET_TAIL_FIELDS];
        tail[2] = self.tail[2].clone(); // trace_id
        tail[4] = self.tail[4].clone(); // sampled
        tail[5] = self.tail[5].clone(); // ingress_id
        tail[6] = self.tail[6].clone(); // sample_rate
        Packet {
            command,
            source: self.destination.clone(),
            destination: self.source.clone(),
            user_id: self.user_id.clone(),
            body,
            oob: Value::None,
            tail, scene_bound: false,
        }
    }

    pub fn error_response(&self, code: i64, payload: Value) -> Packet {
        self.response(cmd::ERRORRESPONSE, vec![Value::Int(self.command), Value::Int(code), payload])
    }
}
