//! PLEX vault reads for `request_messenger.py` and vault history requests, plus
//! the committed-balance notice. Balances and the append-only ledger in
//! game.plex_vault_transactions are read through the exact DAO, not World; the
//! `kind = 'opening'` baseline never reaches these APIs. Wire amounts are the
//! shared clamped projection; the stored ledger stays exact. This handler does
//! not enable client vault UI beyond these four requests and one notice.
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::dao::accounts::plex;
use super::{context::HandlerContext, error::{FailureCode, GatewayError}, message::{child, identifier, message, msg_field, response_for, set}, notices::GatewayNotice, protocol::target_group::Target};

const BALANCE: &str = "eve_public.plex.vault.api.BalanceRequest";
const ALL_LOGGED: &str = "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest";
const LOG: &str = "eve_public.plex.vault.transaction.api.GetLogRequest";
const STATISTICS: &str = "eve_public.plex.vault.transaction.api.GetStatisticsRequest";
const NOTICE: &str = "eve_public.plex.vault.api.BalanceChangedNotice";
const ATTRIBUTES: &str = "eve_public.plex.vault.transaction.Attributes";
const CURRENCY: &str = "eve_public.plex.Currency";

/// Exact numeric text from the ledger to i128. Only a genuine positive or
/// negative overflow of a well-formed integer clamps to the matching i128
/// endpoint, which the shared projection then caps for the wire; malformed text
/// stays an internal error.
fn exact_whole(text: &str) -> Result<i128, GatewayError> {
    match text.parse::<i128>() {
        Ok(whole) => Ok(whole),
        Err(error) => {
            let digits = text.strip_prefix('-').unwrap_or(text);
            if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
                match error.kind() {
                    std::num::IntErrorKind::PosOverflow => return Ok(i128::MAX),
                    std::num::IntErrorKind::NegOverflow => return Ok(i128::MIN),
                    _ => {}
                }
            }
            Err(GatewayError::Internal(error.into()))
        }
    }
}

/// `eve_public.plex.Currency` with the shared signed projection applied.
fn currency(whole: i128) -> anyhow::Result<DynamicMessage> {
    let mut currency = DynamicMessage::new(message(CURRENCY).ok_or_else(|| anyhow::anyhow!("missing descriptor {CURRENCY}"))?);
    set(&mut currency, "total_in_cents", PV::I64(crate::services::plex_vault::projected_cents(whole)))?;
    Ok(currency)
}

/// `google.protobuf.Timestamp` child of `field`, preserving FILETIME 100ns ticks.
fn timestamp(parent: &prost_reflect::MessageDescriptor, field: &str, ticks: i64) -> anyhow::Result<DynamicMessage> {
    let (seconds, nanos) = crate::client::unix_parts_from_filetime(ticks);
    let mut timestamp = child(parent, field)?;
    set(&mut timestamp, "seconds", PV::I64(seconds))?;
    set(&mut timestamp, "nanos", PV::I32(nanos))?;
    Ok(timestamp)
}

/// `eve_public.plex.vault.transaction.Attributes` from one committed ledger row.
fn attributes(created: i64, delta: i128, balance_after: i128) -> anyhow::Result<DynamicMessage> {
    let mut attributes = DynamicMessage::new(message(ATTRIBUTES).ok_or_else(|| anyhow::anyhow!("missing descriptor {ATTRIBUTES}"))?);
    let ts = timestamp(&attributes.descriptor(), "timestamp", created)?;
    set(&mut attributes, "timestamp", PV::Message(ts))?;
    set(&mut attributes, "amount_transferred", PV::Message(currency(delta)?))?;
    set(&mut attributes, "resulting_balance", PV::Message(currency(balance_after)?))?;
    Ok(attributes)
}

/// Committed vault change for every app bound to the account user.
pub(crate) fn balance_changed_notice(user_id: i64, transaction_id: i64, created: i64, delta: i128, balance_after: i128) -> Result<GatewayNotice, GatewayError> {
    let mut payload = DynamicMessage::new(message(NOTICE).ok_or_else(|| GatewayError::Internal(anyhow::anyhow!("missing descriptor {NOTICE}")))?);
    let id = identifier(&payload.descriptor(), "identifier", PV::I64(transaction_id)).map_err(GatewayError::Internal)?;
    set(&mut payload, "identifier", PV::Message(id)).map_err(GatewayError::Internal)?;
    set(&mut payload, "attributes", PV::Message(attributes(created, delta, balance_after).map_err(GatewayError::Internal)?)).map_err(GatewayError::Internal)?;
    let target = Target::User(i32::try_from(user_id).map_err(|error| GatewayError::Internal(error.into()))?);
    GatewayNotice::new(target, &payload)
}

/// The four claimed read requests; anything else is a routing bug.
pub(super) async fn handle(ctx: &HandlerContext<'_>, name: &str, req: &DynamicMessage) -> Result<DynamicMessage, GatewayError> {
    let user = ctx.require_user()?;
    let mut response = response_for(name).map_err(GatewayError::Internal)?;
    if name == BALANCE {
        set(&mut response, "balance", PV::Message(currency(exact_whole(&plex::balance(ctx.db, user).await?)?).map_err(GatewayError::Internal)?)).map_err(GatewayError::Internal)?;
    } else if name == ALL_LOGGED {
        let ids = plex::logged_ids(ctx.db, user).await?;
        let mut transactions = Vec::with_capacity(ids.len());
        for id in ids {
            transactions.push(PV::Message(identifier(&response.descriptor(), "transactions", PV::I64(id)).map_err(GatewayError::Internal)?));
        }
        set(&mut response, "transactions", PV::List(transactions)).map_err(GatewayError::Internal)?;
    } else if name == LOG {
        let sequential = msg_field(req, "identifier").and_then(|m| m.get_field_by_name("sequential").and_then(|v| v.as_i64())).filter(|id| *id > 0)
            .ok_or_else(|| GatewayError::refusal(FailureCode::BadRequest))?;
        let row = plex::logged_transaction(ctx.db, user, sequential).await?
            .ok_or_else(|| GatewayError::refusal(FailureCode::NotFound))?;
        let attributes = attributes(row.created, exact_whole(&row.delta)?, exact_whole(&row.balance_after)?).map_err(GatewayError::Internal)?;
        set(&mut response, "transaction", PV::Message(attributes)).map_err(GatewayError::Internal)?;
        set(&mut response, "unavailable", PV::Bool(true)).map_err(GatewayError::Internal)?;
    } else if name == STATISTICS {
        let stats = plex::statistics(ctx.db, user).await?;
        let (entries, earliest) = match stats.earliest_created {
            None => (Vec::new(), (0, 0)),
            Some(created) => {
                let mut entry = child(&response.descriptor(), "entries").map_err(GatewayError::Internal)?;
                set(&mut entry, "no_classification", PV::Bool(true)).map_err(GatewayError::Internal)?;
                set(&mut entry, "incomes", PV::Message(currency(exact_whole(&stats.incomes)?).map_err(GatewayError::Internal)?)).map_err(GatewayError::Internal)?;
                set(&mut entry, "expenses", PV::Message(currency(exact_whole(&stats.expenses)?).map_err(GatewayError::Internal)?)).map_err(GatewayError::Internal)?;
                let count = u32::try_from(stats.transactions_count).map_err(|error| GatewayError::Internal(error.into()))?;
                set(&mut entry, "transactions_count", PV::U32(count)).map_err(GatewayError::Internal)?;
                (vec![PV::Message(entry)], crate::client::unix_parts_from_filetime(created))
            }
        };
        set(&mut response, "entries", PV::List(entries)).map_err(GatewayError::Internal)?;
        let mut earliest_timestamp = child(&response.descriptor(), "earliest_transaction").map_err(GatewayError::Internal)?;
        set(&mut earliest_timestamp, "seconds", PV::I64(earliest.0)).map_err(GatewayError::Internal)?;
        set(&mut earliest_timestamp, "nanos", PV::I32(earliest.1)).map_err(GatewayError::Internal)?;
        set(&mut response, "earliest_transaction", PV::Message(earliest_timestamp)).map_err(GatewayError::Internal)?;
    } else {
        return Err(GatewayError::Internal(anyhow::anyhow!("unclaimed plex vault request {name}")));
    }
    Ok(response)
}

/// Representative encodings catch field and type drift the request/response
/// existence loop in `handlers::validate` cannot see.
pub(super) fn validate() -> anyhow::Result<()> {
    let mut log = response_for(LOG)?;
    set(&mut log, "transaction", PV::Message(attributes(0, -1, 1)?))?;
    set(&mut log, "unavailable", PV::Bool(true))?;
    let mut stats = response_for(STATISTICS)?;
    let mut entry = child(&stats.descriptor(), "entries")?;
    set(&mut entry, "no_classification", PV::Bool(true))?;
    set(&mut entry, "incomes", PV::Message(currency(1)?))?;
    set(&mut entry, "expenses", PV::Message(currency(-1)?))?;
    set(&mut entry, "transactions_count", PV::U32(0))?;
    set(&mut stats, "entries", PV::List(vec![PV::Message(entry)]))?;
    let mut earliest = child(&stats.descriptor(), "earliest_transaction")?;
    set(&mut earliest, "seconds", PV::I64(0))?;
    set(&mut earliest, "nanos", PV::I32(0))?;
    set(&mut stats, "earliest_transaction", PV::Message(earliest))?;
    let mut balance = response_for(BALANCE)?;
    set(&mut balance, "balance", PV::Message(currency(0)?))?;
    let mut all = response_for(ALL_LOGGED)?;
    let id = identifier(&all.descriptor(), "transactions", PV::I64(1))?;
    set(&mut all, "transactions", PV::List(vec![PV::Message(id)]))?;
    balance_changed_notice(1, 1, 0, 5, 15).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok(())
}

#[cfg(all(test, feature = "tests-gateway"))] mod tests;
