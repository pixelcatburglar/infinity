//! Unlimited-cargo state of the fitted ship's capacity override and its wire
//! representation. Server-side admission compares a typed limit instead of this
//! value; the client is told one large finite capacity so its own volume checks
//! never trigger. Ordinary value and base evaluation keep the real finite
//! numbers; only explicitly wire-facing readers see the representation.

use super::{Fit, Key};
use crate::dao::items::dogma::OverrideState;
use infinity_core::consts::dogma::ATTR_CAPACITY;
use infinity_core::consts::CATEGORY_SHIP;

/// Capacity reported to clients while a hull's unlimited cargo mode is active.
/// Exactly 2^166, which binary64 represents precisely; it is a wire
/// representation choice of this server, not an admission quota. It is intended
/// to exceed any reachable real cargo volume while staying far below
/// floating-point overflow in the client's capacity arithmetic.
pub(crate) const UNLIMITED_CARGO_WIRE_CAPACITY: f64 = f64::from_bits(1189_u64 << 52);

impl Fit {
    /// The real hull's item ID when this fit describes a category-ship node.
    /// Character-only fits, projected models built by `for_space_type` and
    /// hulls whose type is not a ship have no cargo identity.
    pub(crate) fn ship_id(&self) -> Option<i64> {
        self.ship_id.filter(|_| {
            self.nodes.get(&Key::Ship)
                .and_then(|node| node.data_position)
                .and_then(|position| self.sde.types.at_position(position).category_id)
                == Some(CATEGORY_SHIP)
        })
    }

    /// Complete persisted override state of one attribute, pairing the numeric
    /// override with the capacity mode. `None` means no persisted row; only the
    /// real ship's capacity attribute carries the mode flag.
    pub(crate) fn override_state(&self, key: Key, attribute: i32) -> Option<OverrideState> {
        let node = self.nodes.get(&key)?;
        let index = node.overrides.binary_search_by_key(&attribute, |entry| entry.0).ok()?;
        Some(OverrideState {
            value: node.overrides[index].1,
            cargo_unlimited: key == Key::Ship && attribute == ATTR_CAPACITY && self.ship_id().is_some() && node.cargo_unlimited,
        })
    }

    /// Whether the real ship's persisted capacity override is unlimited.
    pub(crate) fn cargo_unlimited(&self) -> bool {
        self.ship_id().is_some() && self.nodes.get(&Key::Ship).is_some_and(|node| node.cargo_unlimited)
    }

    /// The value sent to clients: the representation constant only for the real
    /// ship's capacity while unlimited mode is on; the real effective value
    /// everywhere else.
    pub(crate) fn wire_value(&self, key: Key, attr: i32) -> f64 {
        if key == Key::Ship && attr == ATTR_CAPACITY && self.cargo_unlimited() {
            return UNLIMITED_CARGO_WIRE_CAPACITY;
        }
        self.value(key, attr)
    }
}
