//! Player-visible wallet amount refusals; wallet policy comes from the server.
//! The client has no dedicated catalogue message for malformed or overflowing ISK.

pub const INVALID_AMOUNT: &str = "ISK 金额无效。";
pub const AMOUNT_TOO_LARGE: &str = "ISK 金额超出钱包限额。";
