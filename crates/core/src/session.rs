//! Server-side mirror of the client's distributed session attributes.
//! `carbon/common/script/net/machoNet.py` session values and internal notification pairs.
//! Online snapshots contain only modeled presence fields, not authoritative permissions.

use marshal::Value;
use std::collections::BTreeMap;
/// Decode the internal session notification's two-element key/value pairs.
pub fn decode_pairs(args: &[Value]) -> Vec<(String, Value)> {
    args.iter().filter_map(|pair| match pair.items()? {
        [key, value] => Some((key.as_text()?.into_owned(), value.clone())),
        _ => None,
    }).collect()
}


#[derive(Clone, Debug, PartialEq)]
pub struct OnlineSession {
    pub character_id: i64,
    pub user_id: Option<i64>,
    pub session_id: i64,
    pub ship_id: Option<i64>,
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
            user_id: session.int("userid").filter(|id| *id > 0),
            session_id: session.sid,
            ship_id: session.int("shipid").filter(|id| *id > 0),
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
    /// Project modeled fields without changing the pilot's identity or generation.
    pub fn apply_updates<'a>(&mut self, updates: impl IntoIterator<Item = (&'a str, &'a Value)>) -> bool {
        let before = self.clone();
        for (key, value) in updates {
            let id = || value.as_int().filter(|id| *id > 0);
            match key {
                "userid" => self.user_id = id(),
                "shipid" => self.ship_id = id(),
                "stationid" => self.station_id = id(),
                // Dock clears solarsystemid, not the canonical solarsystemid2.
                "solarsystemid" if !matches!(value, Value::None) => self.system_id = id(),
                "solarsystemid2" => self.system_id = id(),
                "corpid" => self.corporation_id = id(),
                "allianceid" => self.alliance_id = id(),
                "warfactionid" => self.faction_id = id(),
                "fleetid" => self.fleet_id = id(),
                "role" => self.role = value.as_int().unwrap_or(0),
                "corprole" => self.corporation_role = value.as_int().unwrap_or(0),
                _ => {}
            }
        }
        *self != before
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
    pub fn attributes(&self) -> impl Iterator<Item = (&str, &Value)> {
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
