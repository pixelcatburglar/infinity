//! Mutation regressions using the loaded SDE and a dedicated disposable database.
use std::{collections::BTreeMap, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use infinity_core::{consts::{dogma::ATTR_HULL_DAMAGE, CATEGORY_SHIP, FLAG_CARGO, FLAG_DRONE_BAY, FLAG_FITTING_FIRST, FLAG_HANGAR, FLAG_SHIP_HANGAR, CARGO_CONTAINER_GROUPS}};
use crate::{dao::{dynamic_items, items::dogma}, db::{Db, outbox::Outbox}, sde::Sde, services::{CallError, fit::base_attribute, keyval, market}};
use marshal::Value;
use super::{dynamic_item_service, mutation::{self, Context}};

mod custody;
mod fitting;
mod container_capacity;

/// Client dogma rack effects: loPower, hiPower and medPower.
const MODULE_RACK_EFFECTS: [(i32, i32); 3] = [(11, FLAG_FITTING_FIRST), (12, infinity_core::consts::FLAG_HI_SLOT_FIRST), (13, infinity_core::consts::FLAG_MED_SLOT_FIRST)];

struct Fixture {
    db: Db, sde: Arc<Sde>, account: i64, owner: i64, station: i64, ship: i64,
    mutator: i64, source: i64, mutator_type: i32, source_type: i32, result_type: i32,
}

impl Fixture {
    async fn new() -> Self {
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = Arc::new(Sde::load(&db).await.unwrap());
        let (mutator_type, source_type, result_type) = sde.types.iter().find_map(|ty| {
            let mutation = sde.dynamic_items.get(ty.type_id)?;
            mutation.mappings.iter().find_map(|(&source, &result)| {
                let data = sde.types.get(source)?;
                let result_data = sde.types.get(result)?;
                (data.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
                    && result_data.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
                    && sde.types.effects(result).iter().any(|effect| MODULE_RACK_EFFECTS.iter().any(|(id, _)| *id == effect.effect_id))
                    && mutation.attributes.keys().any(|id| sde.types.attributes(source).any(|(attr, _)| attr == *id))
                    && sde.types.attributes(source).any(|(id, _)| !mutation.attributes.contains_key(&id) && id != ATTR_HULL_DAMAGE))
                    .then_some((ty.type_id, source, result))
            })
        }).expect("module dynamic mapping with mutated and inherited attributes in loaded SDE");
        let station = sde.map.stations().next().expect("station in loaded SDE").station_id;
        let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).expect("ship in loaded SDE").type_id;
        let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
        let owner: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,balance) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,0) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
        let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,$6) RETURNING item_id";
        let ship = db.query_one(insert, &[&ship_type, &owner, &station, &FLAG_HANGAR, &1_i64, &true]).await.unwrap().get("item_id");
        let source = db.query_one(insert, &[&source_type, &owner, &station, &FLAG_HANGAR, &1_i64, &true]).await.unwrap().get("item_id");
        let mutator = db.query_one(insert, &[&mutator_type, &owner, &station, &FLAG_HANGAR, &3_i64, &false]).await.unwrap().get("item_id");
        Self { db, sde, account, owner, station, ship, mutator, source, mutator_type, source_type, result_type }
    }

    fn context(&self) -> Context { Context { character: self.owner, station: Some(self.station), ship: Some(self.ship), system: None } }
    fn draws(&self) -> BTreeMap<i32, f64> { mutation::draws(&self.sde, self.mutator_type, || 0.25).ok().unwrap() }

    async fn fingerprint(&self) -> Vec<String> {
        self.db.query("SELECT row_to_json(snapshot.*)::text AS value FROM (SELECT i.*, COALESCE((SELECT json_agg(a ORDER BY a.attribute_id) FROM game.item_attributes a WHERE a.item_id=i.item_id),'[]'::json) AS attrs, (SELECT row_to_json(d) FROM game.dynamic_items d WHERE d.item_id=i.item_id) AS metadata, COALESCE((SELECT json_agg(m ORDER BY m.attribute_id) FROM game.dynamic_item_modifiers m WHERE m.item_id=i.item_id),'[]'::json) AS modifiers FROM game.items i WHERE i.owner_id=$1 ORDER BY i.item_id) snapshot", &[&self.owner]).await.unwrap().into_iter().map(|row| row.get("value")).collect()
    }

    async fn create(&self, context: Context, mutator: i64, source: i64, fail: bool, dispatched: Arc<AtomicBool>) -> Result<i64, CallError> {
        let _lock = market::lock().await;
        let sde = self.sde.clone();
        let mutator = crate::services::items::load_items(&self.db, &sde, crate::dao::items::loading::ItemQuery::Id(mutator)).await.map_err(crate::services::db_err)?.into_iter().next().ok_or_else(|| crate::services::notify_error(infinity_core::messages::dynamic_items::INVALID_ITEMS))?;
        let source = crate::services::items::load_items(&self.db, &sde, crate::dao::items::loading::ItemQuery::Id(source)).await.map_err(crate::services::db_err)?.into_iter().next().ok_or_else(|| crate::services::notify_error(infinity_core::messages::dynamic_items::INVALID_ITEMS))?;
        let draws = self.draws();
        self.db.transaction(move |tx, outbox: &mut Outbox| {
            let sde = sde.clone(); let context = context.clone(); let draws = draws.clone();
            let mutator = mutator.clone(); let source = source.clone();
            Box::pin(async move {
                let result = mutation::create(tx, &sde, outbox, &context, &mutator, &source, &draws).await?;
                if fail { return Err(CallError::Internal(anyhow::anyhow!("injected rollback"))); }
                Ok(result)
            })
        }, move |result, outbox| async move {
            dispatched.store(true, Ordering::SeqCst);
            assert!(!outbox.into_entries().is_empty());
            Ok(result)
        }).await
    }

    async fn cleanup(&self) {
        self.db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=(SELECT location_id FROM game.items WHERE item_id=$1) AND owner_id=$3", &[&self.source, &self.owner, &(self.owner + 1)]).await.unwrap();
        self.db.execute("DELETE FROM game.trades WHERE char_a=$1", &[&self.owner]).await.unwrap();
        self.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&self.ship]).await.unwrap();
        self.db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&self.owner]).await.unwrap();
        self.db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&self.owner]).await.unwrap();
        self.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&self.account]).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE"]
async fn fixed_draws_inherit_source_and_validate_boundaries() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let (mutator, mutation) = sde.types.iter().find_map(|ty| sde.dynamic_items.get(ty.type_id).filter(|m| m.attributes.len() >= 2).map(|m| (ty.type_id, m))).expect("mapping with independent attributes");
    let source = *mutation.mappings.keys().next().unwrap();
    let upper = f64::from_bits(1.0_f64.to_bits() - 1);
    let mut samples = [0.0, upper].into_iter().cycle();
    let draws = mutation::draws(&sde, mutator, || samples.next().unwrap()).ok().unwrap();
    assert_eq!(draws.values().copied().collect::<Vec<_>>(), (0..draws.len()).map(|i| if i % 2 == 0 { 0.0 } else { upper }).collect::<Vec<_>>());
    let (attributes, factors) = mutation::attributes(&sde, source, mutation, &draws).ok().unwrap();
    let data = sde.types.get(source).unwrap();
    for (&id, range) in &mutation.attributes {
        let expected = range.min + draws[&id] * (range.max - range.min);
        assert_eq!(factors[&id], expected);
        assert_eq!(attributes[&id], base_attribute(Some(data), sde.dogma.attribute(id), id) * expected);
    }
    for (id, value) in sde.types.attributes(source) {
        if !mutation.attributes.contains_key(&id) && id != ATTR_HULL_DAMAGE { assert_eq!(attributes[&id], value); }
    }
    for bad in [1.0, -0.1, f64::NAN] {
        let mut invalid = draws.clone(); *invalid.values_mut().next().unwrap() = bad;
        assert!(mutation::attributes(&sde, source, mutation, &invalid).is_err());
    }
    let mut missing = draws.clone(); missing.pop_first();
    assert!(mutation::attributes(&sde, source, mutation, &missing).is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn creation_persists_info_and_rejections_and_injected_failure_roll_back() {
    let fixture = Fixture::new().await;
    let f = Arc::new(fixture);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        let context = f.context();
        let dispatched = Arc::new(AtomicBool::new(false));
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, true, dispatched.clone()).await, Err(CallError::Internal(_))));
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(f.fingerprint().await, before);
        for (ctx, mutator, source) in [
            (Context { character: f.owner + 1, ..context.clone() }, f.mutator, f.source),
            (context.clone(), f.mutator, f.mutator),
            (context.clone(), f.mutator, i64::MAX),
            (Context { station: Some(f.station + 1), ..context.clone() }, f.mutator, f.source),
        ] {
            let before = f.fingerprint().await;
            assert!(matches!(f.create(ctx, mutator, source, false, dispatched.clone()).await, Err(CallError::User { .. })));
            assert_eq!(f.fingerprint().await, before);
        }
        for (column, value) in [("singleton", "false".to_string()), ("quantity", "2".to_string()), ("type_id", "0".to_string()), ("flag_id", FLAG_FITTING_FIRST.to_string())] {
            let statement = format!("UPDATE game.items SET {column}={value} WHERE item_id=$1");
            f.db.execute(&statement, &[&f.source]).await.unwrap();
            let before = f.fingerprint().await;
            assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
            assert_eq!(f.fingerprint().await, before);
            f.db.execute("UPDATE game.items SET singleton=true,quantity=1,type_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &f.source_type, &FLAG_HANGAR]).await.unwrap();
        }
        f.db.execute("UPDATE game.items SET quantity=-1 WHERE item_id=$1", &[&f.mutator]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET quantity=3 WHERE item_id=$1", &[&f.mutator]).await.unwrap();
        dogma::set_override(&f.db, f.source, ATTR_HULL_DAMAGE, 1.0).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("DELETE FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2", &[&f.source, &ATTR_HULL_DAMAGE]).await.unwrap();
        // The same production core must accept local ship cargo, and reject a ship
        // whose actual row is remote or foreign even when the context names it.
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &f.ship, &FLAG_CARGO]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, true, dispatched.clone()).await, Err(CallError::Internal(_))));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&f.source, &FLAG_DRONE_BAY]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, true, dispatched.clone()).await, Err(CallError::Internal(_))));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&f.source, &FLAG_SHIP_HANGAR]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&f.source, &FLAG_CARGO]).await.unwrap();
        f.db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&f.ship, &(f.station + 1)]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET location_id=$2,owner_id=$3 WHERE item_id=$1", &[&f.ship, &f.station, &(f.owner + 1)]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&f.ship, &f.owner]).await.unwrap();
        f.db.execute("INSERT INTO game.trades (trade_id,station_id,char_a,char_b,state,created_at,updated_at) VALUES ($1,$2,$3,$3,'open',0,0)", &[&f.ship, &f.station, &f.owner]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&f.ship]).await.unwrap();
        let container_type = f.sde.types.iter().find(|ty| ty.group_id.is_some_and(|group| CARGO_CONTAINER_GROUPS.contains(&group))).expect("cargo container in SDE").type_id;
        let container: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&container_type, &f.owner, &f.station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let nested: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&container_type, &f.owner, &container, &FLAG_CARGO]).await.unwrap().get("item_id");
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &nested, &FLAG_CARGO]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, true, dispatched.clone()).await, Err(CallError::Internal(_))));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&nested, &(f.owner + 1)]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&nested, &f.owner]).await.unwrap();
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&container, &nested, &FLAG_CARGO]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(context.clone(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &f.station, &FLAG_HANGAR]).await.unwrap();
        let mutation = f.sde.dynamic_items.get(f.mutator_type).unwrap();
        let (attributes, factors) = mutation::attributes(&f.sde, f.source_type, mutation, &f.draws()).ok().unwrap();
        let large_quantity = i64::from(i32::MAX) + 2;
        f.db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&f.mutator, &large_quantity]).await.unwrap();
        let result = f.create(context, f.mutator, f.source, false, dispatched.clone()).await.ok().unwrap();
        assert!(dispatched.load(Ordering::SeqCst));
        let row = f.db.query_one("SELECT type_id,quantity,singleton,location_id,flag_id FROM game.items WHERE item_id=$1", &[&result]).await.unwrap();
        assert_eq!(row.get::<_, i32>("type_id"), f.result_type);
        assert_eq!(row.get::<_, i64>("quantity"), 1);
        assert!(row.get::<_, bool>("singleton"));
        assert_eq!(row.get::<_, i64>("location_id"), f.station);
        assert_eq!(row.get::<_, i32>("flag_id"), FLAG_HANGAR);
        assert!(f.db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&f.source]).await.unwrap().is_none());
        assert_eq!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&f.mutator]).await.unwrap().get::<_, i64>("quantity"), large_quantity - 1);
        let reopened = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let metadata = dynamic_items::get(&reopened, result).await.unwrap().unwrap();
        assert_eq!((metadata.source_item_id, metadata.mutator_item_id, metadata.creator_id), (f.source, f.mutator, f.owner));
        assert_eq!((metadata.source_type_id, metadata.mutator_type_id, metadata.result_type_id), (f.source_type, f.mutator_type, f.result_type));
        assert_eq!(metadata.modifiers, factors);
        let persisted: BTreeMap<_, _> = dogma::overrides(&reopened, result).await.unwrap().into_iter().map(|r| (r.attribute_id, r.value)).collect();
        assert_eq!(persisted, attributes);
        let map = |values: &BTreeMap<i32, f64>| Value::Dict(values.iter().map(|(&id, &value)| (Value::Int(i64::from(id)), Value::Float(value))).collect());
        assert_eq!(dynamic_item_service::info(&reopened, &f.sde, result).await.ok().unwrap(), keyval([
            ("itemID", Value::Int(result)), ("typeID", Value::Int(i64::from(f.result_type))),
            ("sourceTypeID", Value::Int(i64::from(f.source_type))), ("mutatorTypeID", Value::Int(i64::from(f.mutator_type))),
            ("modifiers", map(&factors)), ("attributes", map(&attributes)), ("damaged", Value::Float(0.0)),
        ]));
        dogma::set_override(&f.db, result, ATTR_HULL_DAMAGE, 1.0).await.unwrap();
        let mut damaged = attributes.clone();
        damaged.insert(ATTR_HULL_DAMAGE, 1.0);
        assert_eq!(dynamic_item_service::info(&reopened, &f.sde, result).await.ok().unwrap(), keyval([
            ("itemID", Value::Int(result)), ("typeID", Value::Int(i64::from(f.result_type))),
            ("sourceTypeID", Value::Int(i64::from(f.source_type))), ("mutatorTypeID", Value::Int(i64::from(f.mutator_type))),
            ("modifiers", map(&factors)), ("attributes", map(&damaged)), ("damaged", Value::Float(1.0)),
        ]));
        assert!(matches!(dynamic_item_service::info(&reopened, &f.sde, i64::MAX).await, Err(CallError::User { .. })));
    }).await;
    f.cleanup().await;
    outcome.unwrap();
}
