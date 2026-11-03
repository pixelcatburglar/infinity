//! Registered Lua inventory and market operations against a dedicated database.
use super::*;

// SDE starter station with seeded NPC ore demand.
const STATION: i64 = 60015001;
// SDE Plagioclase, sold to the seeded NPC bid.
const ORE: i32 = 18;
const STACK: i32 = 10;
// Region-wide market order range from marketutil.
const REGION_RANGE: i64 = 32767;
const SOLD: i32 = 3;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE and seeded NPC bids"]
async fn registered_inventory_and_sale_use_real_services() {
    let r = fixture::resources().await;
    let (race, bloodline, school, _) = fixture::creation_inputs(&r).await;
    let fields = fixture::FieldSnapshot::capture(&r, STATION).await;
    let lua = Lua::new();
    register(&lua, &lua.globals(), &r).unwrap();
    let globals = lua.globals();
    globals.set("race", race).unwrap();
    globals.set("bloodline", bloodline).unwrap();
    globals.set("school", school).unwrap();
    globals.set("station", STATION).unwrap();
    globals.set("pilot_name", format!("TradePilot{}", crate::space::now_ms())).unwrap();
    let id: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("id", id).unwrap();
    let peer_id = Arc::new(Mutex::new(None));
    let foreign_id = Arc::new(Mutex::new(None));
    let test_peer = peer_id.clone();
    let test_foreign = foreign_id.clone();
    let test_r = r.clone();
    let result = tokio::spawn(async move {
        let r = test_r;
        let run: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
            globals.set("pilot_name", format!("TradePeer{}", crate::space::now_ms()))?;
            let peer: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await?;
            *test_peer.lock().unwrap() = Some(peer);
            globals.set("peer", peer)?;
            let foreign: i64 = r.db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('trade-foreign-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"test"[..]]).await?.get("user_id");
            *test_foreign.lock().unwrap() = Some(foreign);
            let at = r.sde.map.station(STATION).expect("fixture station in SDE");
            let region = at.region_id.expect("fixture station region");
            let bids = crate::dao::market::orders::bids(&r.db, 0, ORE, region, 0.01).await?;
            let bid = bids.first().expect("seeded NPC ore bid");
            assert!(bid.char_id.is_none(), "highest ore bid must be NPC-owned in dedicated fixture database");
            assert!(bid.at.id == STATION || bid.range >= REGION_RANGE, "NPC bid must reach fixture station");
            assert!(bid.price >= 0.01 && bid.min_volume <= SOLD);
            let bid_id = bid.id;
            let bid_price = bid.price;
            let bid_volume = bid.vol_remaining;
            let bid_escrow = bid.escrow;
            globals.set("price", bid_price)?;
            for source in ["pilots.move_item(id,1,station,nil,nil)", "pilots.sell(id,1,station,price,1,0)"] {
                assert!(lua.load(source).exec_async().await.is_err(), "offline: {source}");
            }
            lua.load("pilots.online(id); pilots.undock(id)").exec_async().await?;
            let identity: Table = lua.load("return pilots.identity(id)").eval_async().await?;
            let ship: i64 = identity.get("ship_id")?;
            let system: i64 = identity.get("system_id")?;
            let item = crate::dao::items::transfers::insert(&r.db, ORE, id, ship, crate::consts::FLAG_CARGO, STACK).await?;
            let foreign_item = crate::dao::items::transfers::insert(&r.db, ORE, peer, STATION, crate::consts::FLAG_HANGAR, STACK).await?;
            let deleted = crate::dao::items::transfers::insert(&r.db, ORE, id, ship, crate::consts::FLAG_CARGO, 1).await?;
            crate::dao::items::inventory::delete(&r.db, deleted).await?;
            globals.set("item", item)?;
            globals.set("foreign_item", foreign_item)?;
            globals.set("deleted", deleted)?;
            let station_pos = r.world.with(system, move |s| s.ball(STATION).map(|b| b.pos)).await.flatten().expect("station actor ball");
            r.world.with(system, move |s| s.set_ball_position(ship, station_pos, |_, _| true)).await.expect("system actor");
            assert!(r.world.after(system, 1100).await);
            lua.load("pilots.dock(id,station)").exec_async().await?;
            let before = item_state(&r.db, item).await?;
            assert_eq!(before, Some((id, ship, crate::consts::FLAG_CARGO, STACK)));
            let foreign_before = item_state(&r.db, foreign_item).await?;
            for source in [
                "pilots.move_item(id,item,station,nil,0)",
                "pilots.move_item(id,item,station,nil,2147483648)",
                "pilots.move_item(id,0,station,nil,nil)",
                "pilots.move_item(id,foreign_item,station,nil,nil)",
                "pilots.move_item(id,item,9223372036854775807,nil,nil)",
                "pilots.move_item(9223372036854775807,item,station,nil,nil)",
            ] {
                assert!(lua.load(source).exec_async().await.is_err(), "invalid move: {source}");
                assert_eq!(item_state(&r.db, item).await?, before);
                assert_eq!(item_state(&r.db, foreign_item).await?, foreign_before);
            }
            // The inventory service intentionally treats a missing item as a no-op.
            lua.load("pilots.move_item(id,deleted,station,nil,nil)").exec_async().await?;
            assert_eq!(item_state(&r.db, item).await?, before);
            r.db.execute("UPDATE game.characters SET user_id=$2 WHERE character_id=$1", &[&id, &foreign]).await?;
            assert!(lua.load("pilots.move_item(id,item,station,nil,nil)").exec_async().await.is_err());
            assert!(lua.load("pilots.sell(id,item,station,price,1,0)").exec_async().await.is_err());
            assert_eq!(item_state(&r.db, item).await?, before);
            r.db.execute("UPDATE game.characters SET user_id=$2, deleted=true WHERE character_id=$1", &[&id, &r.pilots.account]).await?;
            assert!(lua.load("pilots.move_item(id,item,station,nil,nil)").exec_async().await.is_err());
            assert!(lua.load("pilots.sell(id,item,station,price,1,0)").exec_async().await.is_err());
            r.db.execute("UPDATE game.characters SET deleted=false WHERE character_id=$1", &[&id]).await?;
            let returns: i64 = lua.load("return select('#', pilots.move_item(id,item,station,nil,nil))").eval_async().await?;
            assert_eq!(returns, 0);
            let hangar = Some((id, STATION, crate::consts::FLAG_HANGAR, STACK));
            assert_eq!(item_state(&r.db, item).await?, hangar);
            let wallet = crate::dao::wallets::characters::balance(&r.db, id).await?;
            for source in [
                "pilots.sell(id,item,station,0,1,0)", "pilots.sell(id,item,station,0/0,1,0)",
                "pilots.sell(id,item,station,1/0,1,0)", "pilots.sell(id,item,station,price,0,0)",
                "pilots.sell(id,item,station,price,2147483648,0)", "pilots.sell(id,0,station,price,1,0)",
                "pilots.sell(id,deleted,station,price,1,0)", "pilots.sell(id,foreign_item,station,price,1,0)",
                "pilots.sell(id,item,9223372036854775807,price,1,0)",
            ] {
                assert!(lua.load(source).exec_async().await.is_err(), "invalid sale: {source}");
                assert_eq!(item_state(&r.db, item).await?, hangar);
                assert_eq!(item_state(&r.db, foreign_item).await?, foreign_before);
                assert_eq!(crate::dao::wallets::characters::balance(&r.db, id).await?, wallet);
                assert!(crate::dao::market::listings::transactions(&r.db, id, 0).await?.is_empty());
                let current = crate::dao::market::orders::bids(&r.db, 0, ORE, region, bid_price).await?;
                let current = current.iter().find(|order| order.id == bid_id).expect("NPC order still present");
                assert_eq!((current.vol_remaining, current.escrow, current.price), (bid_volume, bid_escrow, bid_price));
            }
            assert!(lua.load("return pilots.sell(id,item,station,price,3,0)").eval_async::<bool>().await?);
            assert_eq!(item_state(&r.db, item).await?, Some((id, STATION, crate::consts::FLAG_HANGAR, STACK - SOLD)));
            assert!(crate::dao::wallets::characters::balance(&r.db, id).await? > wallet);
            let transactions = crate::dao::market::listings::transactions(&r.db, id, 0).await?;
            assert_eq!(transactions.len(), 1);
            let transaction = &transactions[0];
            assert_eq!((transaction.seller_id, transaction.type_id, transaction.quantity, transaction.station_id), (id, i64::from(ORE), i64::from(SOLD), STATION));
            assert_eq!(transaction.price, bid_price);
            assert_eq!(transaction.buyer_id, r.sde.map.station(bid.at.id).and_then(|s| s.owner_id).unwrap_or(0));
            let after = crate::dao::market::orders::bids(&r.db, 0, ORE, region, bid_price).await?;
            let after = after.iter().find(|order| order.id == bid_id).expect("unlimited NPC bid remains");
            assert_eq!((after.vol_remaining, after.escrow, after.price), (bid_volume, bid_escrow, bid_price));
            assert!(crate::dao::market::orders::my_orders(&r.db, id, 0).await?.is_empty());
            Ok(())
        }.await;
        run.unwrap();
    }).await;
    r.db.execute("UPDATE game.characters SET user_id=$2, deleted=false WHERE character_id=$1", &[&id, &r.pilots.account]).await.unwrap();
    let _ = lifecycle::offline(r.clone(), id).await;
    // All market rows are scoped to the newly created seller, including failure after commit.
    r.db.execute("DELETE FROM game.market_transactions WHERE seller_id=$1", &[&id]).await.unwrap();
    r.db.execute("DELETE FROM game.market_order_history WHERE char_id=$1", &[&id]).await.unwrap();
    r.db.execute("DELETE FROM game.market_orders WHERE char_id=$1", &[&id]).await.unwrap();
    let peer = *peer_id.lock().unwrap();
    if let Some(peer) = peer { fixture::cleanup(&r.db, peer).await; }
    fixture::cleanup(&r.db, id).await;
    let foreign = *foreign_id.lock().unwrap();
    if let Some(foreign) = foreign { r.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&foreign]).await.unwrap(); }
    fields.restore(&r).await;
    result.unwrap();
}

async fn item_state(db: &crate::db::Db, id: i64) -> Result<Option<(i64, i64, i32, i32)>, crate::db::Error> {
    Ok(db.query_opt("SELECT owner_id, location_id, flag_id, quantity FROM game.items WHERE item_id=$1", &[&id]).await?
        .map(|row| (row.get("owner_id"), row.get("location_id"), row.get("flag_id"), row.get("quantity"))))
}
