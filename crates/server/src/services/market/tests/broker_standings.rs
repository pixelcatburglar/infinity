//! Database coverage of broker fees on real market operations.
use super::*;
use crate::services::tests::{client, fixture};

async fn fingerprint(db: &Db, character: i64) -> String {
    db.query_one("SELECT json_build_object('wallet', (SELECT row_to_json(c) FROM game.characters c WHERE character_id=$1), 'orders', (SELECT json_agg(o ORDER BY order_id) FROM game.market_orders o WHERE char_id=$1), 'items', (SELECT json_agg(i ORDER BY item_id) FROM game.items i WHERE owner_id=$1))::text AS fingerprint", &[&character]).await.unwrap().get("fingerprint")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn broker_standings_placement_and_repricing() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), f.character).await;
        let row = c.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some() && s.owner_id.and_then(|id| c.sde.npc_corporations.get(id)).and_then(|corp| corp.faction_id).is_some()).expect("NPC station with faction");
        let station_id = row.station_id;
        let owner = row.owner_id.unwrap();
        let faction = c.sde.npc_corporations.get(owner).unwrap().faction_id.unwrap();
        let other = c.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some() && s.owner_id.is_some_and(|id| id != owner && c.sde.npc_corporations.get(id).and_then(|corp| corp.faction_id).is_some_and(|id| id != faction))).expect("NPC station with distinct owner and faction");
        let other_station = other.station_id;
        let other_owner = other.owner_id.unwrap();
        let other_faction = c.sde.npc_corporations.get(other_owner).unwrap().faction_id.unwrap();
        let other_location = (other.solar_system_id.unwrap(), other.constellation_id.unwrap(), other.region_id.unwrap());
        let occupied: std::collections::HashSet<i32> = test_db.query("SELECT DISTINCT type_id FROM game.market_orders", &[]).await.unwrap().iter().map(|r| r.get("type_id")).collect();
        let mut types = c.sde.types.iter().filter(|t| t.type_id > 0 && !occupied.contains(&t.type_id));
        let sell_type = types.next().expect("unlisted SDE type").type_id;
        let buy_type = types.next().expect("second unlisted SDE type").type_id;
        let other_type = types.next().expect("third unlisted SDE type").type_id;
        test_db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, 5, 0)", &[&f.character, &SKILL_BROKER_RELATIONS]).await.unwrap();
        crate::dao::characters::standings::set(&test_db, f.character, owner, 5.0).await.unwrap();
        crate::dao::characters::standings::set(&test_db, f.character, faction, 3.0).await.unwrap();
        assert_eq!(station_broker_rate(&test_db, &c.sde, f.character, owner, 0.015).await.unwrap(), adjusted_broker_rate(0.015, 3.0, 5.0));
        assert_eq!(station_broker_rate(&test_db, &c.sde, f.character, -1, 0.015).await.unwrap(), 0.015);
        test_db.execute("UPDATE game.characters SET balance=2000000 WHERE character_id=$1", &[&f.character]).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&sell_type, &f.character, &station_id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        assert!(place_sell(&Identity::of_client(&c), &c.db, &c.world, &c.sde, item, station_id, sell_type as i64, 1_000_000.0, 1, 1).await.unwrap());
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 1_986_900.0);
        let remaining: i32 = test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get("quantity");
        assert_eq!(remaining, 1);
        let sell = orders::my_orders(&test_db, f.character, filetime_now()).await.unwrap().pop().unwrap();
        assert_eq!((sell.at.owner, sell.vol_remaining, sell.price, sell.escrow), (0, 1, 1_000_000.0, 0.0));
        modify(&c, sell.id, 1_200_000.0).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 1_976_420.0);
        modify(&c, sell.id, 1_000_000.0).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 1_969_870.0);
        test_db.execute("UPDATE game.characters SET balance=1013099 WHERE character_id=$1", &[&f.character]).await.unwrap();
        let before = fingerprint(&test_db, f.character).await;
        assert!(matches!(place_buy(&c, station_id, buy_type as i64, 1_000_000.0, 1, RANGE_STATION, 1, 1).await, Err(CallError::User { .. })));
        assert_eq!(fingerprint(&test_db, f.character).await, before);
        test_db.execute("UPDATE game.characters SET balance=1013100 WHERE character_id=$1", &[&f.character]).await.unwrap();
        place_buy(&c, station_id, buy_type as i64, 1_000_000.0, 1, RANGE_STATION, 1, 1).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 0.0);
        let buy = orders::my_orders(&test_db, f.character, filetime_now()).await.unwrap().into_iter().find(|o| o.bid).unwrap();
        assert_eq!((buy.vol_remaining, buy.escrow), (1, 1_000_000.0));
        assert!(test_db.query("SELECT 1 FROM game.standings WHERE char_id=$1 AND from_id=ANY($2)", &[&f.character, &vec![other_owner, other_faction]]).await.unwrap().is_empty());
        test_db.execute("UPDATE game.characters SET balance=2000000 WHERE character_id=$1", &[&f.character]).await.unwrap();
        place_buy(&c, other_station, other_type as i64, 1_000_000.0, 1, RANGE_STATION, 1, 1).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 985_000.0);
        let other_buy = orders::my_orders(&test_db, f.character, filetime_now()).await.unwrap().into_iter().find(|o| o.type_id == other_type).unwrap();
        assert_eq!((other_buy.at.id, other_buy.at.system, other_buy.at.constellation, other_buy.at.region), (other_station, other_location.0, other_location.1, other_location.2));
        assert_eq!((other_buy.bid, other_buy.vol_remaining, other_buy.escrow), (true, 1, 1_000_000.0));
        modify(&c, other_buy.id, 1_200_000.0).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 773_000.0);
        let repriced = orders::my_orders(&test_db, f.character, filetime_now()).await.unwrap().into_iter().find(|o| o.id == other_buy.id).unwrap();
        assert_eq!((repriced.price, repriced.escrow), (1_200_000.0, 1_200_000.0));
        test_db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
        assert_eq!(station_broker_rate(&test_db, &c.sde, f.character, owner, 0.015).await.unwrap(), 0.015);
        test_db.execute("UPDATE game.characters SET balance=100000 WHERE character_id=$1", &[&f.character]).await.unwrap();
        modify(&c, sell.id, 1_200_000.0).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 88_000.0);
        assert_eq!(orders::my_orders(&test_db, f.character, filetime_now()).await.unwrap().into_iter().find(|o| o.id == sell.id).unwrap().price, 1_200_000.0);
        crate::dao::characters::standings::set(&test_db, f.character, owner, -5.0).await.unwrap();
        crate::dao::characters::standings::set(&test_db, f.character, faction, -3.0).await.unwrap();
        assert_eq!(broker_fee(station_broker_rate(&test_db, &c.sde, f.character, owner, 0.015).await.unwrap(), 1_000_000.0), 16_900.0);
        modify(&c, sell.id, 1_000_000.0).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), 79_550.0);
        assert_eq!(orders::my_orders(&test_db, f.character, filetime_now()).await.unwrap().into_iter().find(|o| o.id == sell.id).unwrap().price, 1_000_000.0);
    }).await;
    db.execute("DELETE FROM game.market_orders WHERE char_id=$1", &[&f.character]).await.unwrap();
    db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    result.unwrap();
}
