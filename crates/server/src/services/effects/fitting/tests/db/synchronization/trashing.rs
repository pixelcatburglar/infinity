//! Trashing fitted resource sources reconciles surviving modules transactionally.
//! Uses the inventory trash entrypoint and real SDE modifier metadata.

use infinity_core::{
    consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HANGAR, FLAG_HI_SLOT_FIRST, FLAG_FITTING_FIRST,
        dogma::{ATTR_CPU, ATTR_CPU_LOAD, ATTR_CPU_OUTPUT, ATTR_POWER, ATTR_POWER_OUTPUT, EFFECT_ONLINE}},
};
use crate::{
    services::{fit::{Fit, Key}, inventory::trashing},
};
use marshal::Value;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn trash_passive_resource_source_reconciles_and_refusal_rolls_back() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station = client.sde.map.stations().next().expect("station in SDE").station_id;
    client.session.apply(vec![("stationid", Value::Int(station)), ("shipid", Value::Int(0))]);
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender;
    let sde = client.sde.clone();
    let ship_type = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP))
        .expect("published ship in SDE").type_id;
    let source_types: Vec<_> = sde.types.iter().filter(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
        && !sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), CATEGORY_MODULE)
        && ty.effects().iter().any(|entry| i64::from(entry.effect_id) != EFFECT_ONLINE
            && sde.dogma.effect(entry.effect_id).is_some_and(|effect| effect.effect_category.unwrap_or(0) == 0
                && effect.modifiers().iter().any(|m| m.modified == ATTR_CPU_OUTPUT && m.domain == "shipID"))))
        .map(|ty| ty.type_id).collect();
    let source_type = *source_types.first().expect("published trashable passive CPU-output module in SDE");
    let survivor_type = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
        && ty.effects().iter().any(|effect| i64::from(effect.effect_id) == EFFECT_ONLINE)
        && crate::services::skills::requirements::requirements(&sde, ty.type_id).next().is_none()
        && !ty.effects().iter().any(|entry| sde.dogma.effect(entry.effect_id)
            .is_some_and(|effect| effect.modifiers().iter().any(|m| m.modified == ATTR_CPU_OUTPUT))))
        .expect("published online module without skill or CPU-output modifiers in SDE").type_id;
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&ship_type, &fixture.character, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    let source: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&source_type, &fixture.character, &ship, &FLAG_FITTING_FIRST]).await.unwrap().get("item_id");
    let survivor: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&survivor_type, &fixture.character, &ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        for (item, attr, value) in [(ship, ATTR_CPU_OUTPUT, 100.0), (ship, ATTR_POWER_OUTPUT, 10000.0), (survivor, ATTR_POWER, 0.0)] {
            assert!(crate::dao::items::dogma::set_override(&db, item, attr, value).await.unwrap());
        }
        let mut boosted = None;
        for source_type in source_types {
            db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&source, &source_type]).await.unwrap();
            let fit = Fit::load(&db, sde.clone(), fixture.character, ship).await.unwrap();
            let output = fit.value(Key::Ship, ATTR_CPU_OUTPUT);
            if output > 100.0 { boosted = Some(output); break; }
        }
        let boosted = boosted.expect("passive SDE module that increases CPU output");
        assert!(boosted > 100.0, "selected passive module must increase CPU output");
        assert!(crate::dao::items::dogma::set_override(&db, survivor, ATTR_CPU, (boosted + 100.0) / 2.0).await.unwrap());
        let before = Fit::load(&db, sde.clone(), fixture.character, ship).await.unwrap();
        assert!(before.value(Key::Ship, ATTR_CPU_LOAD) <= before.value(Key::Ship, ATTR_CPU_OUTPUT));
        assert!(before.value(Key::Ship, ATTR_CPU_LOAD) > 100.0, "surviving module must exceed unboosted output");
        let item = crate::services::items::load_items(&db, &sde, crate::dao::items::loading::ItemQuery::Id(source)).await.unwrap().into_iter().next().unwrap();
        let removed = crate::dao::items::loading::Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..item }.row();
        assert!(trashing::trash_items(&mut client, &[source, source + 9_000_000_000], station).await.is_err());
        assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("n"), 1);
        assert_eq!(crate::dao::items::dogma::online(&db, ship).await.unwrap().len(), 1);
        assert!(client.outbox.is_empty());
        assert!(events.try_recv().is_err());
        trashing::trash_items(&mut client, &[source], station).await.unwrap();
        assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("n"), 0);
        assert!(crate::dao::items::dogma::online(&db, ship).await.unwrap().is_empty());
        let after = Fit::load(&db, sde, fixture.character, ship).await.unwrap();
        let final_cpu = after.value(Key::Ship, ATTR_CPU_OUTPUT);
        assert_eq!(final_cpu, 100.0);
        assert!(after.value(Key::Ship, ATTR_CPU_LOAD) <= final_cpu);
        let mut removed_seen = false;
        let mut offline_seen = false;
        let mut terminal = 0;
        while let Ok((event, args)) = events.try_recv() {
            if event == "OnItemChange" && args.first() == Some(&removed) { removed_seen = true; }
            if event == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(survivor)) && args.get(3).and_then(Value::as_int) == Some(0) {
                assert!(removed_seen, "inventory removal must precede cascade");
                offline_seen = true;
            }
            if event != "OnModuleAttributeChanges" { continue; }
            let [Value::List(changes)] = args.as_slice() else { continue };
            for change in changes {
                let Value::Tuple(row) = change else { continue };
                if row.get(1) == Some(&Value::Int(fixture.character)) && row.get(2) == Some(&Value::Int(ship))
                    && row.get(3) == Some(&Value::Int(i64::from(ATTR_CPU_OUTPUT))) {
                    assert!(removed_seen && offline_seen, "removal and cascade must precede terminal fitting");
                    assert_eq!(row.get(5), Some(&Value::Float(final_cpu)));
                    assert_eq!(row.get(6), Some(&Value::Float(boosted)));
                    terminal += 1;
                }
            }
        }
        assert_eq!(terminal, 1, "one final CPU-output change");
    });
    let outcome = result.await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    outcome.unwrap();
}
