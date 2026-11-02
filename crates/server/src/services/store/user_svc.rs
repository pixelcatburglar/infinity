//! userSvc redeem queue for eve/client/script/ui/services/redeemsvc.py.
//! Uses game redeem tokens, characters, items and SDE types and stations. Invalid
//! tokens remain queued; database write failures roll back the entire claim.
//! Licenses and ISK rewards are not supported.
use super::*;
/// Skill points granted per redeemed type (redeem/data.py `get_redeem_data`).
const SKILL_POINT_TYPES: &[(i32, i64)] = &[
    (60550, 100), (60551, 500), (54652, 1000), (54653, 2500), (63303, 4000), (63304, 4500), (54650, 5000), (63305, 5500),
    (63306, 6000), (63307, 6500), (63308, 7000), (54654, 7500), (63309, 8000), (63310, 9000), (52520, 10000), (63311, 11000),
    (63312, 12000), (63313, 13000), (63314, 14000), (63315, 15000), (63316, 16000), (63317, 17000), (63318, 18000),
    (63623, 19000), (63624, 20000), (63319, 21000), (63320, 22000), (63321, 23000), (63322, 24000), (49756, 25000),
    (63323, 27000), (63324, 28000), (63325, 29000), (57003, 30000), (63326, 31000), (63327, 32000), (63328, 33000),
    (63329, 34000), (63330, 35000), (63331, 42000), (63332, 43000), (63333, 44000), (63334, 45000), (63335, 46000),
    (63336, 47000), (63337, 48000), (63338, 49000), (49809, 50000), (63339, 51000), (63340, 52000), (63824, 75000),
    (49810, 100000), (63825, 125000), (54648, 150000), (43680, 250000), (52269, 500000), (89839, 648000), (52270, 750000),
    (52318, 750000), (52263, 1000000), (56707, 1620000),
];
async fn redeem_tokens(c: &Client) -> CallResult {
    let rows = crate::dao::store::purchases::tokens(&c.db, c.login.user_id).await.map_err(db_err)?;
    let tokens = rows.iter().map(|r| {
        keyval([
            ("tokenID", Value::Int(r.token_id)),
            ("massTokenID", Value::None),
            ("typeID", Value::Int(r.type_id as i64)),
            ("quantity", Value::Int(r.quantity as i64)),
            ("blueprintRuns", Value::None),
            ("blueprintMaterialLevel", Value::None),
            ("blueprintProductivityLevel", Value::None),
            ("stationID", Value::None),
            ("expireDateTime", Value::None),
            ("dateTime", Value::Int(r.created)),
            ("label", s(&r.label)),
            ("description", s(&r.description)),
            ("soulbound", Value::Bool(false)),
        ])
    });
    // (tokens, soulbound (tokenID, massTokenID) pairs)
    Ok(Value::tuple([Value::list(tokens), Value::list([])]))
}

/// Token keys from redeemsvc include pairs as well as dictionary and KeyVal forms.
fn token_ids(v: Option<&Value>) -> Vec<i64> {
    v.and_then(|v| v.items())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| match t {
            Value::Tuple(items) | Value::List(items) => items.first(),
            Value::Instance(_, state) => state.get("tokenID"),
            other => other.get("tokenID"),
        })
        .filter_map(|v| v.as_int())
        .collect()
}

/// ClaimRedeemTokens(tokens, charID, useHomeStation=False).
async fn claim_tokens(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ids = token_ids(call.args.first());
    let char_id = call.args.get(1).and_then(|v| v.as_int()).or_else(|| c.session.int("charid")).unwrap_or(0);
    let home = call.kwargs.get("useHomeStation").or_else(|| call.args.get(2)).and_then(|v| v.as_int()).unwrap_or(0) != 0;
    let user = c.login.user_id;
    let selected = c.session.int("charid") == Some(char_id);
    let current_station = c.session.int("stationid");
    let db = c.db.clone();
    let sde = c.sde.clone();
    let ids: std::sync::Arc<[i64]> = ids.into();
    db.transaction(
        |tx, outbox| {
            let ids = ids.clone();
            let sde = sde.clone();
            Box::pin(async move { claim_in_transaction(tx, outbox, &sde, user, char_id, &ids, home, selected, current_station).await })
        },
        |value, outbox| async move { crate::db::outbox::dispatch_client(value, outbox, c) },
    ).await
}

async fn claim_in_transaction(
    tx: &tokio_postgres::Transaction<'_>,
    outbox: &mut crate::db::outbox::Outbox,
    sde: &crate::sde::Sde,
    user: i64,
    char_id: i64,
    ids: &[i64],
    home: bool,
    selected: bool,
    current_station: Option<i64>,
) -> CallResult {
    use crate::dao::store::purchases;
    let row = purchases::stations(tx, char_id, user).await.map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
    let docked = if selected { current_station } else { row.station_id };
    let station = if home { row.home_station_id } else { docked.or(row.home_station_id) };
    let tokens = purchases::lock_tokens(tx, user, ids).await.map_err(db_err)?;
    let mut valid = Vec::new();
    let mut errors = Vec::new();
    for token in &tokens {
        let is_sp = SKILL_POINT_TYPES.iter().any(|(id, _)| *id == token.type_id);
        if sde.types.get(token.type_id).is_none()
            || (!is_sp && station.and_then(|id| sde.map.station(id)).is_none())
        {
            errors.push(redeem_error(token.token_id));
        } else {
            valid.push(token);
        }
    }
    let mut successful = Vec::new();
    let mut created = Vec::new();
    let mut skill_points = 0i64;
    for token in valid {
        if let Some((_, sp)) = SKILL_POINT_TYPES.iter().find(|(id, _)| *id == token.type_id) {
            skill_points += sp * i64::from(token.quantity);
            successful.push(token.token_id);
            continue;
        }
        let destination = station.expect("validated item destination");
        let item_id = crate::dao::items::transfers::insert(tx, token.type_id, char_id, destination, FLAG_HANGAR, token.quantity).await.map_err(db_err)?;
        successful.push(token.token_id);
        created.push(Value::dict([
            ("itemID", Value::Int(item_id)),
            ("typeID", Value::Int(i64::from(token.type_id))),
            ("quantity", Value::Int(i64::from(token.quantity))),
            ("stationID", Value::Int(destination)),
        ]));
        if selected && current_station == station {
            crate::services::inventory::notify_item_change(tx, sde, outbox, item_id, vec![(crate::consts::IX_LOCATION_ID, Value::Int(0))]).await?;
        }
    }
    if purchases::trash(tx, user, &successful).await.map_err(db_err)? != successful.len() as u64 {
        return Err(db_err(anyhow::anyhow!("redeem token deletion mismatch")));
    }
    if skill_points > 0 {
        let free = purchases::add_points(tx, char_id, skill_points).await.map_err(db_err)?;
        if selected {
            outbox.notify_client("OnFreeSkillPointsChanged", vec![Value::Int(free)]);
        }
    }
    Ok(Value::dict([
        ("errors", Value::list(errors)),
        ("itemsCreated", Value::list(created)),
        ("stationID", station.map(Value::Int).unwrap_or(Value::None)),
        ("isk_balance", Value::None),
        ("activated_licenses", Value::list([])),
        ("isk_rewarded", Value::Int(0)),
    ]))
}

fn redeem_error(token_id: i64) -> Value {
    let error = crate::client::user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::store::REDEEM_FAILED))]);
    Value::tuple([Value::Int(token_id), error])
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("userSvc", "GetRedeemTokens") => redeem_tokens(c).await,
        ("userSvc", "ClaimRedeemTokens") => claim_tokens(c, call).await,
        ("userSvc", "TrashRedeemTokens") => {
            let ids = token_ids(call.args.first());
            match crate::dao::store::purchases::trash(&c.db, c.login.user_id, &ids).await {
                Ok(_) => Ok(Value::None),
                Err(e) => Err(db_err(e)),
            }
        }

        _ => return None,
    })
}

#[cfg(test)] mod tests;
