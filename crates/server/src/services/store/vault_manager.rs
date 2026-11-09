//! vaultManager PLEX balance, backed by game.accounts; other currencies are not modelled.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        // get_account_balance(currency): PLEX in the vault; no other currencies.
        ("vaultManager", "get_account_balance") => {
            if call.args.first().and_then(|v| v.as_text()).as_deref() == Some(PLX) {
                crate::dao::accounts::balances::plex_balance(&c.db, c.login.user_id).await.map_err(db_err).map(Value::Int)
            } else {
                Ok(Value::Int(0))
            }
        }

        _ => return None,
    })
}
