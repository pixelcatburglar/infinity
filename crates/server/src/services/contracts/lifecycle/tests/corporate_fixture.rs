//! Shared real database fixtures for corporate contract lifecycle tests.
use super::super::*;
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_ACCOUNT_CAN_TAKE_1, ROLE_CONTRACT_MANAGER, ROLE_HANGAR_CAN_TAKE_1};
use crate::services::tests::fixture;

#[derive(Clone, Copy)]
pub(super) struct Corp {
    pub actor: fixture::Fixture,
    pub ceo: fixture::Fixture,
    pub id: i64,
    pub office: i64,
}

pub(super) async fn setup(db: &Db, station: i64, station_type: i32) -> Corp {
    let actor = fixture::create(db).await.unwrap();
    let ceo = fixture::create(db).await.unwrap();
    let id: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('contract-test-' || nextval('game.corporations_corporation_id_seq')::text,'CT' || nextval('game.corporations_corporation_id_seq')::text,$1,$1,$2,0) RETURNING corporation_id", &[&ceo.character, &station]).await.unwrap().get("corporation_id");
    let roles = ROLE_ACCOUNTANT | ROLE_CONTRACT_MANAGER | ROLE_HANGAR_CAN_TAKE_1 | ROLE_ACCOUNT_CAN_TAKE_1 | (ROLE_ACCOUNT_CAN_TAKE_1 << 1) | (ROLE_ACCOUNT_CAN_TAKE_1 << 2);
    db.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$2::bigint & ~$5::bigint,corp_roles_hq=$2::bigint & $5::bigint,station_id=$3,balance=1000000 WHERE character_id=$4", &[&id, &roles, &station, &actor.character, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=$2,corp_roles=$4 WHERE character_id=$3", &[&id, &station, &ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    for key in [ACCOUNT_CASH, ACCOUNT_CASH + 1, ACCOUNT_CASH + 2] {
        db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,1000000)", &[&id, &key]).await.unwrap();
    }
    let office = transfers::insert(db, station_type, id, station, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::offices::insert(db, office, id, station, 1).await.unwrap();
    Corp { actor, ceo, id, office }
}

pub(super) fn session(c: &mut Client, corp: Corp, station: i64, system: i64, key: i32) {
    c.session.apply(vec![("corpid", Value::Int(corp.id)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system)), ("corpAccountKey", Value::Int(i64::from(key)))]);
}

pub(super) fn payload(kind: i32, station: i64, end: i64, flag: i32, root: i64, price: f64, reward: f64, collateral: f64) -> Value {
    Value::dict([
        ("contractType", Value::Int(i64::from(kind))), ("startStationID", Value::Int(station)),
        ("destinationID", Value::Int(end)), ("startStationDivision", Value::Int(i64::from(flag))),
        ("minutesExpire", Value::Int(1440)), ("numDays", Value::Int(1)),
        ("price", Value::Float(price)), ("reward", Value::Float(reward)), ("collateral", Value::Float(collateral)),
        ("forCorp", Value::Bool(true)), ("itemList", Value::list([Value::tuple([Value::Int(root), Value::Int(1)])])),
        ("requestItemTypeList", Value::list([])),
    ])
}

pub(super) fn id(result: CallResult) -> i64 {
    match result { Ok(value) => value.items().unwrap()[0].as_int().unwrap(), Err(_) => panic!("contract creation failed") }
}

pub(super) async fn fingerprint(db: &Db, c: Corp) -> String {
    db.query_one("SELECT json_build_object('items',(SELECT json_agg(row_to_json(i) ORDER BY item_id) FROM game.items i WHERE owner_id=ANY($2)), 'wallets',(SELECT json_agg(row_to_json(w) ORDER BY account_key) FROM game.corp_wallets w WHERE corporation_id=$1), 'characters',(SELECT json_agg(row_to_json(c) ORDER BY character_id) FROM game.characters c WHERE character_id=ANY($3)), 'contracts',(SELECT json_agg(row_to_json(k) ORDER BY contract_id) FROM game.contracts k WHERE issuer_corp_id=$1 OR acceptor_id=$1))::text AS state", &[&c.id, &vec![c.id, c.actor.character, c.ceo.character], &vec![c.actor.character, c.ceo.character]]).await.unwrap().get("state")
}

pub(super) async fn cleanup(db: &Db, c: Corp) {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        fixture::cleanup_progress(tx, &[c.id], &[]).await
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("DELETE FROM game.contract_bids WHERE contract_id IN (SELECT contract_id FROM game.contracts WHERE issuer_corp_id=$1 OR acceptor_id=$1)", &[&c.id]).await.unwrap();
    db.execute("DELETE FROM game.contract_items WHERE contract_id IN (SELECT contract_id FROM game.contracts WHERE issuer_corp_id=$1 OR acceptor_id=$1)", &[&c.id]).await.unwrap();
    db.execute("DELETE FROM game.contracts WHERE issuer_corp_id=$1 OR acceptor_id=$1", &[&c.id]).await.unwrap();
    db.execute("DELETE FROM game.blueprints WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=ANY($1))", &[&vec![c.id, c.actor.character, c.ceo.character]]).await.unwrap();
    db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&c.office]).await.unwrap();
    db.execute("DELETE FROM game.industry_jobs WHERE installer_id=ANY($1)", &[&vec![c.actor.character, c.ceo.character]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&c.id]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&c.id]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=ANY($1)", &[&vec![c.actor.character, c.ceo.character]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&c.id]).await.unwrap();
    fixture::cleanup(db, c.actor).await.unwrap();
    fixture::cleanup(db, c.ceo).await.unwrap();
}

pub(super) use infinity_core::consts::{FLAG_CORP_DELIVERIES as DELIVERIES, FLAG_CORP_SAG1 as SAG};
