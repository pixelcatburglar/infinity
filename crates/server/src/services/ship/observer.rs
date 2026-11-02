//! Transport callbacks for a docking pilot.

use crate::services::CallError;
use std::future::Future;

/// `None::<&mut Client>` selects the observer type without constructing a client.
pub(crate) trait DockObserver: Send {
    fn accepted(&mut self, station: i64);
    fn wait_pumping(&mut self, system: i64, delay_ms: u64) -> impl Future<Output = Result<bool, CallError>> + Send;
    fn unobserve(&mut self);
}
