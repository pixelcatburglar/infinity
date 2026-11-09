//! Corporation persistence for registry, roles, enlistment, shares, dividends and votes.
//! Services mirror `eve/client/script/ui/services/corporation` and voting UI calls.
//! Sources include corporations, characters, share holdings, receipts and vote tables.
//! Policy, transactions, notifications and sanctioned vote actions are not implemented here.

pub(crate) mod audit;
pub(crate) mod members;
pub(crate) mod enlistment;
pub(crate) mod dividends;
pub(crate) mod shares;
pub(crate) mod votes;
pub(crate) mod lookup;
pub(crate) mod registry;
pub(crate) mod dispatch;
pub(crate) mod roles;
pub(crate) mod pending_kicks;
pub(crate) mod tax;
pub(crate) mod wars;
