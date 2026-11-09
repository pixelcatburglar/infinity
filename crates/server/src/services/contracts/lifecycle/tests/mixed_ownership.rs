//! Contract escrow rejects mixed-owner trees through the real service dispatcher.
use super::super::*;
use super::corporate_fixture as f;
use crate::dao::items::{creation, dogma, pending_reloads};

async fn rpc(c: &mut Client, request: Value) -> CallResult {
    let args = [request, Value::Bool(false)];
    let kwargs = Value::None;
    crate::services::dispatch(c, &Call { service: "contractProxy", method: "CreateContract", args: &args, kwargs: &kwargs, bound: false, bind_params: None }).await
}
async fn contract_items(db: &Db, owner: i64) -> Vec<(i64, Option<i64>, i64, i64, bool)> {
    let contracts = crate::dao::contracts::query::list(db, crate::dao::contracts::query::Filter::Issuer(owner, STATUS_OUTSTANDING)).await.unwrap();
    let ids: Vec<_> = contracts.iter().map(|contract| contract.id).collect();
    let mut items: Vec<_> = shared::items(db, &ids).await.unwrap().into_iter()
        .map(|item| (item.contract_id, item.item_id, item.type_id, item.quantity, item.in_crate)).collect();
    items.sort_unstable();
    items
}


fn request(station: i64, flag: i32, corporate: bool, assignee: i64, roots: &[i64], multi: bool) -> Value {
    Value::dict([
        ("contractType", Value::Int(i64::from(TYPE_ITEM_EXCHANGE))),
        ("startStationID", Value::Int(station)),
        ("startStationDivision", Value::Int(i64::from(flag))),
        ("minutesExpire", Value::Int(1440)),
        ("forCorp", Value::Bool(corporate)),
        ("assignedToID", Value::Int(assignee)),
        ("multiContract", Value::Bool(multi)),
        ("itemList", Value::list(roots.iter().map(|id| Value::tuple([Value::Int(*id), Value::Int(1)])).collect::<Vec<_>>())),
        ("requestItemTypeList", Value::list([])),
    ])
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn mixed_descendants_refuse_personal_corporate_and_multigroup_escrow() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (station_id, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let kind = sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && t.published == Some(true)).unwrap().type_id;
    let corp = f::setup(&db, station_id, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut client, _peer) = crate::services::tests::client(db.clone(), corp.actor.character).await;
        f::session(&mut client, corp, station_id, system, ACCOUNT_CASH + 1);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(corp.actor.character, tx);
        let (guest_tx, mut guest_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(corp.ceo.character, guest_tx);
        for corporate in [false, true] {
            assert!(rx.try_recv().is_err(), "actor baseline before corporate={corporate}");
            assert!(guest_rx.try_recv().is_err(), "guest baseline before corporate={corporate}");
            let owner = if corporate { corp.id } else { corp.actor.character };
            let location = if corporate { corp.office } else { station_id };
            let flag = if corporate { f::SAG } else { FLAG_HANGAR };
            let good = creation::rookie_ship(&db, kind, owner, location, flag).await.unwrap();
            let root = creation::rookie_ship(&db, kind, owner, location, flag).await.unwrap();
            let good_child = creation::rookie_ship(&db, kind, owner, good, FLAG_HANGAR).await.unwrap();
            let child = creation::rookie_ship(&db, kind, owner, root, FLAG_HANGAR).await.unwrap();
            let guest = creation::rookie_ship(&db, kind, corp.ceo.character, child, FLAG_HANGAR).await.unwrap();
            assert!(dogma::set_override(&db, guest, infinity_core::consts::dogma::ATTR_DAMAGE_MULTIPLIER, 1.0).await.unwrap());
            let reload = pending_reloads::insert(&db, corp.ceo.character, guest, guest, FLAG_HANGAR, kind, 1, FLAG_HANGAR, child).await.unwrap();
            let ids = [good, good_child, root, child, guest];
            let before = f::fingerprint(&db, corp).await;
            let contract_items_before = contract_items(&db, owner).await;
            let attributes = dogma::overrides_for(&db, &ids).await.unwrap().into_iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>();
            let pending = pending_reloads::intersecting(&db, &ids).await.unwrap();
            for (roots, multi) in [(vec![root], false), (vec![good, root], true)] {
                match rpc(&mut client, request(station_id, flag, corporate, if corporate { 0 } else { corp.id }, &roots, multi)).await {
                    Err(CallError::User { msg, dict }) => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::CONTRACT_ITEM_MUST_BE_IN_START_HANGAR))]);
                    }
                    _ => panic!("expected mixed-owner contract refusal"),
                }
                assert_eq!(f::fingerprint(&db, corp).await, before);
                assert_eq!(contract_items(&db, owner).await, contract_items_before);
                assert!(pending_reloads::intersecting(&db, &ids).await.unwrap() == pending);
                assert_eq!(dogma::overrides_for(&db, &ids).await.unwrap().into_iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>(), attributes);
                assert!(rx.try_recv().is_err(), "actor notice after refusal corporate={corporate} multi={multi}");
                assert!(guest_rx.try_recv().is_err(), "guest notice after refusal corporate={corporate} multi={multi}");
            }
            pending_reloads::take(&db, reload).await.unwrap();
            dogma::clear_overrides(&db, guest).await.unwrap();
            let result = rpc(&mut client, request(station_id, flag, corporate, if corporate { 0 } else { corp.id }, &[good], false)).await;
            let contract = f::id(result);
            let escrow = shared::items(&db, &[contract]).await.unwrap();
            assert_eq!(escrow.len(), 1);
            assert_eq!(escrow[0].item_id, Some(good));
            assert_eq!(escrow[0].quantity, 1);
            let rows = load_items(&db, &client.sde, ItemQuery::Id(good)).await.unwrap();
            assert_eq!((rows[0].owner_id, rows[0].location_id), (owner, -contract));
            while rx.try_recv().is_ok() {}
            if !corporate {
                assert_eq!(guest_rx.try_recv().unwrap(), ("OnContractAssigned".to_string(), vec![Value::Int(contract)]));
                assert!(guest_rx.try_recv().is_err(), "unexpected guest notice after personal success");
            }
            let child_rows = load_items(&db, &client.sde, ItemQuery::Id(good_child)).await.unwrap();
            assert_eq!((child_rows[0].owner_id, child_rows[0].location_id), (owner, good));
        }
        client.world.unregister_pilot_generation(corp.actor.character, None);
        client.world.unregister_pilot_generation(corp.ceo.character, None);
    }).await;
    f::cleanup(&db, corp).await;
    result.unwrap();
}
