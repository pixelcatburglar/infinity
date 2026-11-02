//! Dedicated database regression for active blueprints in requested exchange items.
use super::super::*;
use super::corporate_fixture as f;

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn requested_nested_active_blueprints_refuse_personal_and_corporate_acceptance() {
    use crate::consts::{INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let root_type = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let bp_type = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(9)).unwrap().type_id;
    let issuer = f::setup(&db, station_id, station_type).await;
    let buyer = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut client, _peer) = crate::services::tests::client(db.clone(), buyer.actor.character).await;
        f::session(&mut client, buyer, station_id, system, ACCOUNT_CASH);
        let (buyer_tx, mut buyer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (issuer_tx, mut issuer_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(buyer.actor.character, buyer_tx);
        client.world.register_pilot(issuer.actor.character, issuer_tx);
        for corporate in [false, true] {
            let owner = if corporate { buyer.id } else { buyer.actor.character };
            let flag = if corporate { f::DELIVERIES } else { FLAG_HANGAR };
            let root = transfers::insert(&db, root_type, owner, station_id, flag, 1).await.unwrap();
            db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&root]).await.unwrap();
            let bp = transfers::insert(&db, bp_type, owner, root, FLAG_HANGAR, 1).await.unwrap();
            db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&bp]).await.unwrap();
            crate::dao::industry::set_blueprint(&db, bp, 0, 0, -1).await.unwrap();
            let job = crate::dao::industry::Job {
                id: 0, installer: buyer.actor.character, owner, activity: 1, blueprint_id: bp,
                blueprint_type: bp_type, blueprint_copy: false, facility: station_id, system,
                runs: 1, licensed_runs: 1, successful_runs: None, product_type: None, optional_type: None,
                cost: 0.0, probability: 1.0, time: 1, start: 0, end: 1, completed: None,
                completed_by: None, status: INDUSTRY_JOB_STATUS_INSTALLED, input_location: station_id,
                input_flag: flag, output_location: station_id, output_flag: flag, wallet_owner: owner,
                wallet_account_key: ACCOUNT_CASH, blueprint_location: root, blueprint_flag: FLAG_HANGAR,
            };
            let job_id = crate::dao::industry::insert_job(&db, &job).await.unwrap();
            let id = contract_dao::insert(&db, &contract_dao::NewContract {
                kind: TYPE_ITEM_EXCHANGE, issuer: issuer.actor.character, issuer_corp: issuer.id,
                for_corp: false, issuer_wallet_key: ACCOUNT_CASH, issuer_hangar_flag: FLAG_HANGAR,
                issuer_source_location: station_id, assignee: 0, start: station_id, end: 0,
                price: 10.0, reward: 0.0, collateral: 0.0, title: "", description: "", num_days: 1,
                paid_deposit: 0.0, auction_tax_rate: 0.0, auction_tax_maximum: 0.0,
                status: STATUS_OUTSTANDING, issued: filetime_now(), expires: i64::MAX,
            }).await.unwrap();
            contract_dao::insert_requested_item(&db, id, root_type, 1).await.unwrap();
            for status in [INDUSTRY_JOB_STATUS_INSTALLED, INDUSTRY_JOB_STATUS_READY] {
                db.execute("UPDATE game.industry_jobs SET status=$2 WHERE job_id=$1", &[&job_id, &status]).await.unwrap();
                let before_buyer = f::fingerprint(&db, buyer).await;
                let before_issuer = f::fingerprint(&db, issuer).await;
                match accept(&client, id, corporate).await {
                    Err(CallError::User { msg, dict }) => {
                        assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(crate::messages::industry::BLUEPRINT_ACTIVE))]);
                    }
                    _ => panic!("expected active blueprint refusal"),
                }
                assert_eq!(f::fingerprint(&db, buyer).await, before_buyer);
                assert_eq!(f::fingerprint(&db, issuer).await, before_issuer);
                let row = db.query_one("SELECT (SELECT status FROM game.contracts WHERE contract_id=$1) AS status, (SELECT location_id FROM game.items WHERE item_id=$2) AS root_location, (SELECT location_id FROM game.items WHERE item_id=$3) AS blueprint_location", &[&id, &root, &bp]).await.unwrap();
                assert_eq!(row.get::<_, i32>("status"), STATUS_OUTSTANDING);
                assert_eq!(row.get::<_, i64>("root_location"), station_id);
                assert_eq!(row.get::<_, i64>("blueprint_location"), root);
                assert!(buyer_rx.try_recv().is_err());
                assert!(issuer_rx.try_recv().is_err());
            }
        }
        client.world.unregister_pilot(buyer.actor.character);
        client.world.unregister_pilot(issuer.actor.character);
    }).await;
    f::cleanup(&db, buyer).await;
    f::cleanup(&db, issuer).await;
    result.unwrap();
}
