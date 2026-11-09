//! Database coverage of broker fees on real market operations.
use super::*;
use super::super::fees::station_broker_rate;
use super::super::trading::{place_buy, place_sell};
use infinity_core::consts::{FLAG_HANGAR, SKILL_BROKER_RELATIONS};
use crate::services::{identity::Identity, wallets::balance};
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
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let stations: Vec<_> = fixture::isolated_market_stations(&test_db, &c.sde).await.unwrap().collect();
        let (row, other) = stations.iter().find_map(|row| {
            let faction = c.sde.npc_corporations.get(row.owner).unwrap().faction_id.unwrap();
            stations.iter().find(|other| other.owner != row.owner && c.sde.npc_corporations.get(other.owner).unwrap().faction_id.unwrap() != faction).map(|other| (*row, *other))
        }).expect("isolated NPC stations with distinct owners and factions");
        let station_id = row.id;
        let owner = row.owner;
        let faction = c.sde.npc_corporations.get(owner).unwrap().faction_id.unwrap();
        let other_station = other.id;
        let other_owner = other.owner;
        let other_faction = c.sde.npc_corporations.get(other_owner).unwrap().faction_id.unwrap();
        let other_location = (other.system, other.constellation, other.region);
        let system = row.system;
        let empty_region = stations.iter().find(|station| station.region != row.region).expect("another isolated region").region;
        let highsec = c.sde.map.system(system).and_then(|s| s.security).is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold);
        let other_highsec = c.sde.map.system(other.system).and_then(|s| s.security).is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold);
        let rate = FeeRate::from_input(0.015).unwrap();
        let amount = |value| crate::isk::IskAmount::from_whole_isk(value).unwrap();
        c.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system))]);
        let mut types = fixture::market_type_ids(&c.sde);
        let sell_type = types.next().expect("tradable market SDE type");
        let buy_type = types.next().expect("second tradable market SDE type");
        let other_type = types.next().expect("third tradable market SDE type");
        drop(types);
        test_db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, 5, 0)", &[&f.character, &SKILL_BROKER_RELATIONS]).await.unwrap();
        crate::dao::characters::standings::set(&test_db, f.character, owner, 5.0).await.unwrap();
        crate::dao::characters::standings::set(&test_db, f.character, faction, 3.0).await.unwrap();
        assert_eq!(station_broker_rate(&test_db, &c.sde, f.character, owner, rate).await.unwrap(), adjusted_broker_rate(rate, 3.0, 5.0).unwrap());
        assert_eq!(station_broker_rate(&test_db, &c.sde, f.character, -1, rate).await.unwrap(), rate);
        test_db.execute("UPDATE game.characters SET balance=2000000 WHERE character_id=$1", &[&f.character]).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&sell_type, &f.character, &station_id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        assert!(place_sell(&Identity::of_client(&c), &c.db, &c.world, &c.sde, SellRequest { item_id: item, station_id, type_id: sell_type as i64, price: 1_000_000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap());
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::from_cents(198_690_000 - if highsec { 5_000_000 } else { 0 }).unwrap());
        let remaining: i64 = test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get("quantity");
        assert_eq!(remaining, 1);
        let sell = orders::my_orders(&test_db, f.character).await.unwrap().pop().unwrap();
        assert_eq!((sell.at.owner, sell.vol_remaining, sell.price, sell.escrow), (0, 1, amount(1_000_000), amount(0)));
        crate::dao::market::tests::age_order_for_modification(&test_db, sell.id).await.unwrap();
        super::super::lifecycle::modify_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, modify_request(&c, sell.id, 1_200_000.0).await).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::from_cents(197_642_000 - if highsec { 5_000_000 } else { 0 }).unwrap());
        crate::dao::market::tests::age_order_for_modification(&test_db, sell.id).await.unwrap();
        super::super::lifecycle::modify_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, modify_request(&c, sell.id, 1_000_000.0).await).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::from_cents(196_987_000 - if highsec { 5_000_000 } else { 0 }).unwrap());
        let required = 1_013_100_i64 + if highsec { 50_000 } else { 0 };
        test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&f.character,&(required - 1).to_string()]).await.unwrap();
        let before = fingerprint(&test_db, f.character).await;
        assert!(matches!(place_buy(&Identity::of_client(&c), &c.db, &c.world, &c.sde, BuyRequest { station_id, type_id: buy_type as i64, price: 1_000_000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await, Err(CallError::User { .. })));
        assert_eq!(fingerprint(&test_db, f.character).await, before);
        test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&f.character,&required.to_string()]).await.unwrap();
        place_buy(&Identity::of_client(&c), &c.db, &c.world, &c.sde, BuyRequest { station_id, type_id: buy_type as i64, price: 1_000_000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::ZERO);
        let buy = orders::my_orders(&test_db, f.character).await.unwrap().into_iter().find(|o| o.bid).unwrap();
        assert_eq!((buy.vol_remaining, buy.escrow), (1, amount(1_000_000)));
        // Exercise the same regional GetOrders route and its complete wire rows.
        let current_sell = orders::my_orders(&test_db, f.character).await.unwrap().into_iter().find(|o| o.id == sell.id).unwrap();
        for order in [&current_sell, &buy] {
            c.session.apply(vec![("regionid", Value::Int(order.at.region))]);
            let kwargs = Value::Dict(vec![]);
            let call = crate::services::Call { service: "marketProxy", method: "GetOrders", args: &[Value::Int(i64::from(order.type_id))], kwargs: &kwargs, bound: false, bind_params: None };
            let wire = cached_result(&crate::services::dispatch(&mut c, &call).await.unwrap(), true);
            let sides = wire.items().unwrap();
            assert_eq!(sides.len(), 2);
            for (index, side) in sides.iter().enumerate() {
                let Value::Reduce { data, list, dict } = side else { panic!("expected CRowset") };
                assert!(list.is_empty() && dict.is_empty());
                let parts = data.items().unwrap();
                assert_eq!(parts[0], Value::global("carbon.common.script.sys.crowset.CRowset"));
                let args = parts[1].items().unwrap();
                let descriptor = DbRowDescriptor::from_value(&args[0]).unwrap();
                assert_eq!(descriptor.columns, vec![
                    ("price".into(), DbType::R8), ("volRemaining".into(), DbType::I8),
                    ("typeID".into(), DbType::I4), ("range".into(), DbType::I2),
                    ("orderID".into(), DbType::I8), ("volEntered".into(), DbType::I8),
                    ("minVolume".into(), DbType::I8), ("bid".into(), DbType::Bool),
                    ("issueDate".into(), DbType::FileTime), ("duration".into(), DbType::I2),
                    ("stationID".into(), DbType::I8), ("regionID".into(), DbType::I4),
                    ("solarSystemID".into(), DbType::I4), ("constellationID".into(), DbType::I4),
                    ("jumps".into(), DbType::I2),
                ]);
                let rows = args[1].items().unwrap();
                if (index == 1) == order.bid {
                    assert_eq!(rows.len(), 1);
                    for (name, expected) in [
                        ("price", Value::Float(order.price.wire_isk())), ("volRemaining", Value::Int(order.vol_remaining.into())),
                        ("typeID", Value::Int(order.type_id.into())), ("range", Value::Int(order.range)),
                        ("orderID", Value::Int(order.id)), ("volEntered", Value::Int(order.vol_entered.into())),
                        ("minVolume", Value::Int(order.min_volume.into())), ("bid", Value::Bool(order.bid)),
                        ("issueDate", Value::Int(order.issued.unwrap_or_else(today))), ("duration", Value::Int(order.duration)),
                        ("stationID", Value::Int(order.at.id)), ("regionID", Value::Int(order.at.region)),
                        ("solarSystemID", Value::Int(order.at.system)), ("constellationID", Value::Int(order.at.constellation)),
                        ("jumps", Value::Int(0)),
                    ] {
                        assert_eq!(descriptor.get(&rows[0], name), Some(expected), "{name}");
                    }
                } else {
                    assert!(rows.is_empty());
                }
            }
            c.session.apply(vec![("regionid", Value::Int(empty_region))]);
            let wrong_region = cached_result(&crate::services::dispatch(&mut c, &call).await.unwrap(), true);
            for side in wrong_region.items().unwrap() {
                let Value::Reduce { data, .. } = side else { panic!("expected CRowset") };
                assert!(data.items().unwrap()[1].items().unwrap()[1].items().unwrap().is_empty());
            }
        }
        assert!(test_db.query("SELECT 1 FROM game.standings WHERE char_id=$1 AND from_id=ANY($2)", &[&f.character, &vec![other_owner, other_faction]]).await.unwrap().is_empty());
        test_db.execute("UPDATE game.characters SET balance=2000000 WHERE character_id=$1", &[&f.character]).await.unwrap();
        c.session.apply(vec![("stationid", Value::Int(other_station)), ("solarsystemid2", Value::Int(other_location.0))]);
        place_buy(&Identity::of_client(&c), &c.db, &c.world, &c.sde, BuyRequest { station_id: other_station, type_id: other_type as i64, price: 1_000_000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::from_cents(98_500_000 - if other_highsec { 5_000_000 } else { 0 }).unwrap());
        let other_buy = orders::my_orders(&test_db, f.character).await.unwrap().into_iter().find(|o| o.type_id == other_type).unwrap();
        assert_eq!((other_buy.at.id, other_buy.at.system, other_buy.at.constellation, other_buy.at.region), (other_station, other_location.0, other_location.1, other_location.2));
        assert_eq!((other_buy.bid, other_buy.vol_remaining, other_buy.escrow), (true, 1, amount(1_000_000)));
        crate::dao::market::tests::age_order_for_modification(&test_db, other_buy.id).await.unwrap();
        super::super::lifecycle::modify_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, modify_request(&c, other_buy.id, 1_200_000.0).await).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::from_cents(77_300_000 - if other_highsec { 5_000_000 } else { 0 }).unwrap());
        let repriced = orders::my_orders(&test_db, f.character).await.unwrap().into_iter().find(|o| o.id == other_buy.id).unwrap();
        assert_eq!((repriced.price, repriced.escrow), (amount(1_200_000), amount(1_200_000)));
        test_db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
        assert_eq!(station_broker_rate(&test_db, &c.sde, f.character, owner, rate).await.unwrap(), rate);
        test_db.execute("UPDATE game.characters SET balance=100000 WHERE character_id=$1", &[&f.character]).await.unwrap();
        c.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system))]);
        crate::dao::market::tests::age_order_for_modification(&test_db, sell.id).await.unwrap();
        super::super::lifecycle::modify_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, modify_request(&c, sell.id, 1_200_000.0).await).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::from_cents(8_800_000).unwrap());
        assert_eq!(orders::my_orders(&test_db, f.character).await.unwrap().into_iter().find(|o| o.id == sell.id).unwrap().price, amount(1_200_000));
        crate::dao::characters::standings::set(&test_db, f.character, owner, -5.0).await.unwrap();
        crate::dao::characters::standings::set(&test_db, f.character, faction, -3.0).await.unwrap();
        assert_eq!(broker_fee(station_broker_rate(&test_db, &c.sde, f.character, owner, rate).await.unwrap(), amount(1_000_000)).unwrap(), amount(16_900));
        crate::dao::market::tests::age_order_for_modification(&test_db, sell.id).await.unwrap();
        super::super::lifecycle::modify_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, modify_request(&c, sell.id, 1_000_000.0).await).await.unwrap();
        assert_eq!(balance(&test_db, f.character).await.unwrap(), crate::isk::IskAmount::from_cents(7_955_000).unwrap());
        assert_eq!(orders::my_orders(&test_db, f.character).await.unwrap().into_iter().find(|o| o.id == sell.id).unwrap().price, amount(1_000_000));
    }).await;
    db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
    cleanup_market_fixture(&db, f).await.unwrap();
    result.unwrap();
}
