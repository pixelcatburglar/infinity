//! Prepared daily-goal mutation and durable registration.
//! Client contribution methods are described by `dailygoals/client/utils.py`;
//! state comes from frozen daily-goal rows and retained authored policies.
//! Source preparation, settlement and account authentication are separate.
pub(crate) mod source;
mod state;
mod apply;
pub(crate) mod admin;
pub(crate) mod payment;
pub(crate) mod prepare;

/// Server policy: one durable consumer across all content versions.
const CONSUMER_KEY: &str = "daily-goals";

pub(crate) fn registration() -> crate::progress::DurableRegistration {
    crate::progress::DurableRegistration {
        consumer_key: CONSUMER_KEY,
        kinds: source::KINDS,
        mode: crate::progress::DeliveryMode::Prepared(prepare::delivery),
    }
}

#[cfg(test)] mod tests;
