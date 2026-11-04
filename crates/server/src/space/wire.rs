//! Destiny ball wire encoding, damage state and slim items.
//! Mirrors carbonengine destiny `Thunkers.cpp` WriteBallToStream, consumed by
//! `eve/client/script/remote/michelle.py`; data comes from transient Ball state.
//! Miniballs, dynamical orientation, formations and unsupported ball modes are not implemented.

use super::{Ball, collision, DAMAGE_STATE_SHIELD_RECHARGE_TAU, FLAG_FREE, FLAG_GLOBAL,
    FLAG_INTERACTIVE, FLAG_MASSIVE, MODE_FOLLOW, MODE_GOTO, MODE_MISSILE, MODE_ORBIT,
    MODE_RIGID, MODE_WARP};
use marshal::Value;

impl Ball {
    pub(super) fn flags(&self, collisions: bool) -> u8 {
        (if self.free { FLAG_FREE } else { 0 })
            | (if self.global { FLAG_GLOBAL } else { 0 })
            | (if collision::massive(self, collisions) { FLAG_MASSIVE } else { 0 })
            | if self.interactive { FLAG_INTERACTIVE } else { 0 }
    }

    /// Ballpark::WriteBallToStream (no miniballs, no dynamical orientation).
    pub(super) fn write(&self, o: &mut Vec<u8>, collisions: bool) {
        o.extend_from_slice(&self.id.to_le_bytes());
        o.push(self.mode);
        o.extend_from_slice(&self.radius.to_le_bytes());
        for c in self.pos {
            o.extend_from_slice(&c.to_le_bytes());
        }
        o.push(self.flags(collisions));
        if self.mode != MODE_RIGID {
            o.extend_from_slice(&self.mass.to_le_bytes());
            o.push(self.cloak); // isCloaked
            o.extend_from_slice(&(-1i64).to_le_bytes()); // harmonic
            o.extend_from_slice(&(self.corp_id as i32).to_le_bytes());
            o.extend_from_slice(&(self.alliance_id as i32).to_le_bytes());
        }
        if self.free {
            o.extend_from_slice(&self.max_vel.to_le_bytes());
            for c in self.vel {
                o.extend_from_slice(&c.to_le_bytes());
            }
            o.extend_from_slice(&self.agility.to_le_bytes());
            o.extend_from_slice(&self.speed_fraction.to_le_bytes());
        }
        o.push(0xFF); // formationID -1
        match self.mode {
            MODE_FOLLOW | MODE_ORBIT => {
                o.extend_from_slice(&self.follow_id.to_le_bytes());
                o.extend_from_slice(&self.follow_range.to_le_bytes());
            }
            MODE_GOTO => {
                for c in self.goto {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
            MODE_MISSILE => {
                o.extend_from_slice(&self.follow_id.to_le_bytes());
                o.extend_from_slice(&self.follow_range.to_le_bytes());
                o.extend_from_slice(&self.launcher.to_le_bytes());
                o.extend_from_slice(&(self.effect_stamp as i32).to_le_bytes()); // effectStamp: launch tick
                for c in self.goto {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
            MODE_WARP => {
                for c in self.goto {
                    o.extend_from_slice(&c.to_le_bytes());
                }
                o.extend_from_slice(&(self.effect_stamp as i32).to_le_bytes());
                o.extend_from_slice(&self.warp.length.to_le_bytes());
                o.extend_from_slice(&self.warp.min_range.to_le_bytes());
                o.extend_from_slice(&self.warp.factor.to_le_bytes());
            }
            _ => {}
        }
    }

    /// `((shield, shieldRechargeTau, time), armor, structure)` (spaceObject.OnDamageState).
    pub fn damage_state(&self) -> Value {
        let [shield, armor, hull] = self.health;
        Value::tuple([
            Value::tuple([Value::Float(shield), Value::Float(DAMAGE_STATE_SHIELD_RECHARGE_TAU), Value::Int(crate::client::filetime_now())]),
            Value::Float(armor),
            Value::Float(hull),
        ])
    }

    pub(super) fn slim_item(&self) -> Value {
        let mut fields: Vec<(&str, Value)> = vec![
            ("itemID", Value::Int(self.id)),
            ("typeID", Value::Int(self.type_id as i64)),
            ("ownerID", Value::Int(self.owner_id)),
        ];
        if let Some(ch) = self.char_id {
            fields.push(("charID", Value::Int(ch)));
            fields.push(("corpID", Value::Int(self.corp_id)));
            fields.push(("allianceID", if self.alliance_id > 0 { Value::Int(self.alliance_id) } else { Value::None }));
            fields.push(("warFactionID", Value::None));
            if !self.slim.iter().any(|(k, _)| *k == "securityStatus") {
                fields.push(("securityStatus", Value::Float(0.0)));
            }
            fields.push(("bounty", Value::Int(0)));
        }
        fields.extend(self.slim.iter().cloned());
        Value::instance("eve.common.script.util.slimItem.SlimItem", fields)
    }
}
