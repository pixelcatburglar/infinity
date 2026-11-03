//! Online guests in the client's `station` service, from the World pilot registry.
//! `eve/client/script/ui/station/base.py` converts the tuple list to a guest map.
//! Offline characters and cross-process presence are not included.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("station", "GetGuests") => {
            let guests = c.session.int("stationid").filter(|id| *id > 0)
                .map(|station| c.world.station_guests(station)).unwrap_or_default();
            Ok(Value::list(guests.iter().map(crate::services::presence::guest_record)))
        }
        // No-op / empty answers for station systems we don't model yet.
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
