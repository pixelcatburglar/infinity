//! Server-side mirror of the client's distributed session attributes.

use marshal::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OnlineSession {
    pub character_id: i64,
    pub station_id: Option<i64>,
    pub system_id: Option<i64>,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub faction_id: Option<i64>,
    pub fleet_id: Option<i64>,
    pub role: i64,
    pub corporation_role: i64,
}

impl OnlineSession {
    pub fn from_session(session: &Session) -> Option<Self> {
        Some(Self {
            character_id: session.int("charid").filter(|id| *id > 0)?,
            station_id: session.int("stationid").filter(|id| *id > 0),
            system_id: session.int("solarsystemid2").filter(|id| *id > 0),
            corporation_id: session.int("corpid").filter(|id| *id > 0),
            alliance_id: session.int("allianceid").filter(|id| *id > 0),
            faction_id: session.int("warfactionid").filter(|id| *id > 0),
            fleet_id: session.int("fleetid").filter(|id| *id > 0),
            role: session.int("role").unwrap_or(0),
            corporation_role: session.int("corprole").unwrap_or(0),
        })
    }
}

pub struct Session {
    pub sid: i64,
    attrs: BTreeMap<String, Value>,
}

impl Session {
    pub fn new(sid: i64) -> Self {
        Session { sid, attrs: BTreeMap::new() }
    }

    pub fn get(&self, key: &str) -> &Value {
        self.attrs.get(key).unwrap_or(&Value::None)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).as_int()
    }

    /// Inspect session attributes without modifying the distributed session.
    pub(crate) fn attributes(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.attrs.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// Apply updates; returns the `{attr: (old, new)}` pairs that actually changed.
    pub fn apply(&mut self, updates: Vec<(&str, Value)>) -> Vec<(Value, Value)> {
        let mut change = Vec::new();
        for (k, new) in updates {
            let old = self.get(k).clone();
            if old != new {
                change.push((Value::str(k), Value::tuple([old, new.clone()])));
                self.attrs.insert(k.to_string(), new);
            }
        }
        change
    }
}

#[cfg(test)]
mod tests;
