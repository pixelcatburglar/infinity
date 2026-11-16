//! Office lifecycle notifications: `OnOfficeRentalChange(corporationID, officeID)`
//! reaches the corporation's live members plus station guests watching the public
//! rental list, and `OnCorpAssetChange(items, stationID)` reaches the management
//! asset audience (Accountant, Junior Accountant, Director or the active CEO) with
//! real nonempty rows. Notifications queue inside the transaction outbox and are
//! best-effort refreshes; a lost push is reconstructed from the database.
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use crate::services::{db_err, CallError};
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_JUNIOR_ACCOUNTANT};
use marshal::Value;

/// Live corporation members authorized for the management asset view.
pub(in crate::services) async fn management_members(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<i64>, CallError> {
    let ids = crate::dao::characters::members::active_ids(ex, corp).await.map_err(db_err)?;
    let authorities = crate::dao::characters::roles::authorities(ex, &ids).await.map_err(db_err)?;
    const MANAGEMENT: i64 = ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT | ROLE_DIRECTOR;
    Ok(authorities.into_iter().filter(|authority|
        crate::services::corps::permissions::allows(authority, corp, MANAGEMENT)
            || crate::services::corps::permissions::active_ceo(authority, authority.character)
    ).map(|authority| authority.character).collect())
}

/// Queue `OnOfficeRentalChange` for every live member of `corp` and every current
/// station guest captured before the transaction.
pub(in crate::services) fn office_rental_notice<X: Send>(outbox: &mut Outbox<X>, corp: i64, office: i64, members: &[i64], guests: &[i64]) {
    let mut recipients: std::collections::BTreeSet<i64> = members.iter().copied().collect();
    recipients.extend(guests.iter().copied());
    for id in recipients {
        outbox.notify_pilot(id, "OnOfficeRentalChange", vec![Value::Int(corp), Value::Int(office)]);
    }
}

/// Queue `OnCorpAssetChange` with the real changed rows; an empty inventory change
/// rides the office notice alone instead of a fake empty asset push.
pub(in crate::services) fn corp_asset_notice<X: Send>(outbox: &mut Outbox<X>, station: i64, rows: Vec<Value>, audience: &[i64]) {
    if rows.is_empty() {
        return;
    }
    for id in audience {
        outbox.notify_pilot(*id, "OnCorpAssetChange", vec![Value::list(rows.iter().cloned()), Value::Int(station)]);
    }
}
