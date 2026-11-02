//! Contract escrow volume regressions against real item rows and loaded SDE.
use super::super::*;
use super::corporate_fixture as f;

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn escrow_volume_uses_root_state_and_selected_quantity() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.type_id.is_some() && s.solar_system_id.is_some()).unwrap();
    let (start, system, station_type) = (station.station_id, station.solar_system_id.unwrap(), i32::try_from(station.type_id.unwrap()).unwrap());
    let ty = sde.types.iter().find(|t| {
        if t.volume.is_none() { return false; }
        let packaged = crate::services::item_volume::item_volume(&sde, t.type_id, false, 1, None);
        let assembled = crate::services::item_volume::item_volume(&sde, t.type_id, true, 1, None);
        matches!((packaged, assembled), (Ok(a), Ok(b)) if a >= 0.0 && b >= 0.0 && a != b)
    }).unwrap().type_id;
    let corp = f::setup(&db, start, station_type).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut client, _peer) = crate::services::tests::client(db.clone(), corp.actor.character).await;
        f::session(&mut client, corp, start, system, ACCOUNT_CASH + 1);
        assert_eq!(volume(&db, &sde, 0).await.unwrap().to_bits(), 0.0f64.to_bits());
        let root = transfers::insert(&db, ty, corp.id, start, f::DELIVERIES, 1).await.unwrap();
        let id = f::id(create(&client, &f::payload(TYPE_ITEM_EXCHANGE, start, 0, f::DELIVERIES, root, 0.0, 0.0, 0.0)).await);
        db.execute("UPDATE game.items SET quantity=10, singleton=false WHERE item_id=$1", &[&root]).await.unwrap();
        db.execute("UPDATE game.contract_items SET quantity=3 WHERE contract_id=$1 AND item_id=$2", &[&id, &root]).await.unwrap();
        let packaged = crate::services::item_volume::item_volume(&sde, ty, false, 10, Some(3)).unwrap();
        assert_eq!(volume(&db, &sde, id).await.unwrap(), packaged);
        db.execute("UPDATE game.items SET quantity=1, singleton=true WHERE item_id=$1", &[&root]).await.unwrap();
        db.execute("UPDATE game.contract_items SET quantity=1 WHERE contract_id=$1 AND item_id=$2", &[&id, &root]).await.unwrap();
        let assembled = crate::services::item_volume::item_volume(&sde, ty, true, 1, Some(1)).unwrap();
        assert_eq!(volume(&db, &sde, id).await.unwrap(), assembled);
        assert_ne!(assembled, packaged / 3.0);
        db.execute("UPDATE game.items SET type_id=$2, quantity=-1234, singleton=true WHERE item_id=$1", &[&root, &TYPE_PLASTIC_WRAP]).await.unwrap();
        let wrapped = crate::services::item_volume::item_volume(&sde, TYPE_PLASTIC_WRAP, true, -1234, Some(1)).unwrap();
        assert_eq!(volume(&db, &sde, id).await.unwrap(), wrapped);
        assert_eq!(wrapped, 12.34);
        db.execute("UPDATE game.contract_items SET item_id=NULL WHERE contract_id=$1 AND item_id=$2", &[&id, &root]).await.unwrap();
        assert!(matches!(volume(&db, &sde, id).await, Err(CallError::Internal(error)) if error.to_string() == "contract escrow item missing"));
    }).await;
    f::cleanup(&db, corp).await;
    result.unwrap();
}
