//! Loyalty offers, purchases, redeem tokens and login reward persistence.
//! The owning store service supports `vgs/client/vgsClient.py`; this DAO has no wire contract.
//! Sources include `game.accounts`, `game.characters`, `game.redeem_tokens`,
//! `game.store_purchases` and `game.login_reward_claims`.
//! Live store catalogue and LP offer tables are unavailable; policy, transactions,
//! delivery and notifications remain with callers.
pub(crate) mod purchases;
pub(crate) mod rewards;
