//! Persisted dynamic-module bases in the ordinary fitting loader.
use super::*;
use crate::{dao::items::{inventory, loading::ItemQuery}, services::{fit::{Fit, Key}, items::load_items}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn created_module_reloads_into_fit_and_consumes_last_mutator() {
    let f = Arc::new(Fixture::new().await);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        f.db.execute("UPDATE game.items SET quantity=1 WHERE item_id=$1", &[&f.mutator]).await.unwrap();
        let dispatched = Arc::new(AtomicBool::new(false));
        let result = f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await.ok().unwrap();
        assert!(dispatched.load(Ordering::SeqCst));
        assert!(f.db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&f.mutator]).await.unwrap().is_none());
        let item = load_items(&f.db, &f.sde, ItemQuery::Id(result)).await.unwrap().into_iter().next().unwrap();
        let ship = f.ship;
        let owner = f.owner;
        let flag = MODULE_RACK_EFFECTS.iter().find(|(effect, _)| f.sde.types.effects(f.result_type).iter().any(|entry| entry.effect_id == *effect)).unwrap().1;
        f.db.transaction(move |tx, _: &mut Outbox| {
            let item = item.clone();
            Box::pin(async move {
                assert!(inventory::move_item(tx, &item, ship, flag, owner, true).await.map_err(crate::services::db_err)?);
                Ok::<(), CallError>(())
            })
        }, |(), _| async move { Ok(()) }).await.ok().unwrap();
        let reopened = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let persisted: BTreeMap<_, _> = dogma::overrides(&reopened, result).await.unwrap().into_iter().map(|row| (row.attribute_id, row.value)).collect();
        let mutation = f.sde.dynamic_items.get(f.mutator_type).unwrap();
        let (expected, _) = mutation::attributes(&f.sde, f.source_type, mutation, &f.draws()).ok().unwrap();
        let mutated = *mutation.attributes.keys().find(|id| f.sde.types.attributes(f.source_type).any(|(attr, _)| attr == **id)).unwrap();
        let inherited = f.sde.types.attributes(f.source_type).find(|(id, _)| !mutation.attributes.contains_key(id) && *id != ATTR_HULL_DAMAGE).unwrap();
        let fit = Fit::load(&reopened, f.sde.clone(), f.owner, f.ship).await.ok().unwrap();
        assert_eq!(fit.base(Key::Module(result), mutated), persisted[&mutated]);
        assert_eq!(persisted[&mutated], expected[&mutated]);
        assert_eq!(persisted[&inherited.0], inherited.1);
        assert_eq!(fit.base(Key::Module(result), inherited.0), inherited.1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let world = crate::space::World::new(reopened.clone(), f.sde.clone(), 1);
        let mut client = crate::client::Client::new(infinity_wire::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), reopened.clone(), f.sde.clone(),
            crate::chat::Hub::new("localhost", reopened.clone(), world.clone()), world, crate::handshake::Login { user_id: f.account, user_name: "dynamic-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        client.session.apply(vec![("charid", Value::Int(f.owner)), ("shipid", Value::Int(f.ship))]);
        let args = [Value::Int(result)];
        let kwargs = Value::Dict(vec![]);
        let call = crate::services::Call { service: "dogmaIM", method: "ItemGetInfo", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let info = crate::services::dogma::dispatch(&mut client, &call).await.ok().unwrap();
        let Value::Dict(attributes) = crate::services::value_field(&info, "attributes").unwrap() else { panic!("item attributes are not a dictionary") };
        assert!(attributes.contains(&(Value::Int(i64::from(mutated)), Value::Float(persisted[&mutated]))));
        assert!(attributes.contains(&(Value::Int(i64::from(inherited.0)), Value::Float(inherited.1))));
    }).await;
    f.cleanup().await;
    outcome.unwrap();
}
