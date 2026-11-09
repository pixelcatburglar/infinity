use super::*;
use crate::services::{effects::online, inventory::{self, access::Container}};
use crate::db::outbox::Outbox;
use tokio::sync::mpsc::UnboundedReceiver;

fn active(id: i64, owner: i64) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    state().active.insert(id, ActiveModule { char_id: owner, stop: stop.clone(), forced_inactive: false });
    stop
}

fn stopped(stop: &AtomicBool) -> bool { stop.load(Ordering::Relaxed) }

fn bank_pushes(rx: &mut UnboundedReceiver<(String, Vec<Value>)>) -> Vec<Vec<Value>> {
    let mut pushes = Vec::new();
    while let Ok((name, args)) = rx.try_recv() {
        if name == "OnWeaponBanksChanged" { pushes.push(args); }
    }
    pushes
}

fn direct_bank_pushes(client: &mut Client) -> Vec<Vec<Value>> {
    std::mem::take(&mut client.outbox).into_iter().filter_map(|packet| {
        let crate::macho::Address::Broadcast { broadcast_id, .. } = packet.destination else { return None };
        if broadcast_id.as_text().as_deref() != Some("OnWeaponBanksChanged") { return None; }
        let envelope = packet.body[0].items().unwrap();
        let decoded = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
        let fields = decoded.items().unwrap();
        Some(fields[1].items().unwrap().to_vec())
    }).collect()
}

fn all_bank_pushes(client: &mut Client, rx: &mut UnboundedReceiver<(String, Vec<Value>)>) -> Vec<Vec<Value>> {
    let mut pushes = bank_pushes(rx);
    pushes.extend(direct_bank_pushes(client));
    pushes
}

pub(super) async fn exercise(db: &Db, client: &mut Client, ship: i64, owner: i64, guns: &[i64], rx: &mut UnboundedReceiver<(String, Vec<Value>)>) {
    db.execute("DELETE FROM game.weapon_banks WHERE ship_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.items SET online=true WHERE location_id=$1", &[&ship]).await.unwrap();
    let (a, b, c, d) = (guns[0], guns[1], guns[2], guns[3]);

    // A standalone active gun and an existing bank both lose their old cycles when joined.
    let old_a = active(a, owner);
    let old_b = active(b, owner);
    assert!(rpc(client, "LinkWeapons", ship, a, b).await.is_ok());
    assert!(stopped(&old_a) && stopped(&old_b));
    let current = active(a, owner);
    assert!(rpc(client, "MergeModuleGroups", ship, a, b).await.is_ok());
    assert!(rpc(client, "PeelAndLink", ship, a, b).await.is_ok());
    assert!(rpc(client, "DestroyWeaponBank", ship, d, 0).await.is_ok());
    assert!(rpc(client, "LinkWeapons", ship, a, ship).await.is_err());
    assert!(!stopped(&current));
    assert!(rpc(client, "LinkWeapons", ship, a, c).await.is_ok());
    assert!(stopped(&current));
    let captured = capture_stop(a).unwrap();
    let replacement = active(a, owner);
    apply_stop(captured);
    assert!(!stopped(&replacement));

    let old_d = active(d, owner);
    assert!(rpc(client, "LinkWeapons", ship, d, guns[4]).await.is_ok());
    assert!(stopped(&old_d));
    let peel_a = active(a, owner);
    let peel_d = active(d, owner);
    assert!(rpc(client, "PeelAndLink", ship, a, d).await.is_ok());
    assert!(stopped(&peel_a) && stopped(&peel_d));
    assert!(rpc(client, "LinkWeapons", ship, d, guns[5]).await.is_ok());
    let old_a = active(a, owner);
    let old_d = active(d, owner);
    assert!(rpc(client, "MergeModuleGroups", ship, a, d).await.is_ok());
    assert!(stopped(&old_a) && stopped(&old_d));
    let old_a = active(a, owner);
    assert!(rpc(client, "UnlinkModule", ship, a, 0).await.is_ok());
    assert!(stopped(&old_a));
    let old_a = active(a, owner);
    assert!(rpc(client, "DestroyWeaponBank", ship, a, 0).await.is_ok());
    assert!(stopped(&old_a));
    assert!(rpc(client, "LinkAllWeapons", ship, 0, 0).await.is_ok());
    let link_all_master = active(a, owner);
    assert!(rpc(client, "LinkAllWeapons", ship, 0, 0).await.is_ok());
    assert!(!stopped(&link_all_master));
    assert!(rpc(client, "UnlinkAllModules", ship, 0, 0).await.is_ok());
    assert!(stopped(&link_all_master));
    assert!(rpc(client, "LinkWeapons", ship, a, b).await.is_ok());
    assert!(rpc(client, "LinkWeapons", ship, a, c).await.is_ok());
    let replacement = active(a, owner);
    assert!(bank_pushes(rx).is_empty());
    assert!(direct_bank_pushes(client).is_empty());
    // A failed transaction discards both the DB mutation and queued side effects.
    let policy = online::OnlinePolicy::from_identity(&crate::services::identity::Identity::of_client(client), client.notify_tx.clone()).ok().expect("online policy");
    let sde = client.sde.clone();
    let fences = online::capture_online_fences(&client.world, &[owner]).await;
    let rollback: Result<(), CallError> = db.transaction(
        |tx, outbox: &mut Outbox<online::OnlineChanged>| {
            let policy = policy.clone();
            let sde = sde.clone();
            let replacement = replacement.clone();
            let fences = fences.clone();
            Box::pin(async move {
                assert!(online::set_online_core(tx, &sde, &policy, outbox, &fences, ship, b, false).await?.0);
                assert!(!stopped(&replacement));
                Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))
            })
        }, |(), _| async { Ok(()) },
    ).await;
    assert!(rollback.is_err());
    assert_eq!(rows(db, ship).await, vec![(a, b), (a, c)]);
    assert!(db.query_one("SELECT online FROM game.items WHERE item_id=$1", &[&b]).await.unwrap().get::<_, bool>("online"));
    assert!(!stopped(&replacement));
    assert!(bank_pushes(rx).is_empty());
    assert!(direct_bank_pushes(client).is_empty());

    // Docked clients still receive the bank map through their own sender.
    client.world.unregister_pilot_generation(owner, None);
    online::set_online(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, ship, b, false, |notification| online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("offline slave");
    assert!(stopped(&replacement));
    assert!(rows(db, ship).await.is_empty());
    assert!(!db.query_one("SELECT online FROM game.items WHERE item_id=$1", &[&b]).await.unwrap().get::<_, bool>("online"));
    assert_eq!(bank_pushes(rx), vec![vec![Value::Int(ship), Value::Dict(vec![])]]);
    online::set_online(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, ship, b, false, |notification| online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("repeat offline slave");
    assert!(bank_pushes(rx).is_empty());
    online::set_online(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, ship, b, true, |notification| online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("online slave");
    assert!(rpc(client, "LinkWeapons", ship, a, b).await.is_ok());
    assert!(rpc(client, "LinkWeapons", ship, a, c).await.is_ok());
    let master = active(a, owner);
    online::set_online(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, ship, a, false, |notification| online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("offline master");
    assert!(stopped(&master));
    assert!(rows(db, ship).await.is_empty());
    assert_eq!(bank_pushes(rx), vec![vec![Value::Int(ship), Value::Dict(vec![])]]);
    online::set_online(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.notify_tx.clone(), client.login.user_id, ship, a, true, |notification| online::dispatch_notification(notification, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("online master");

    assert!(rpc(client, "LinkWeapons", ship, a, b).await.is_ok());
    assert!(rpc(client, "LinkWeapons", ship, a, c).await.is_ok());
    let master = active(a, owner);
    let cargo = Container::Item(ship);
    assert!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &cargo, b, Some(ship + 1), Some(infinity_core::consts::FLAG_CARGO), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
    assert_eq!(rows(db, ship).await, vec![(a, b), (a, c)]);
    assert!(!stopped(&master));
    assert!(bank_pushes(rx).is_empty());
    assert!(direct_bank_pushes(client).is_empty());
    inventory::movement::move_item(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &cargo, b, Some(ship), Some(28), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("move slave to same slot");
    assert!(!stopped(&master));
    assert!(bank_pushes(rx).is_empty());
    assert!(direct_bank_pushes(client).is_empty());
    inventory::movement::move_item(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &cargo, b, Some(ship), Some(infinity_core::consts::FLAG_CARGO), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("move slave to cargo");
    assert_eq!(rows(db, ship).await, vec![(a, c)]);
    assert!(stopped(&master));
    assert_eq!(all_bank_pushes(client, rx), vec![vec![Value::Int(ship), Value::Dict(vec![(Value::Int(a), Value::List(vec![Value::Int(c)]))])]]);
    let master = active(a, owner);
    inventory::movement::move_item(&crate::services::identity::Identity::of_client(client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &cargo, a, Some(ship), Some(infinity_core::consts::FLAG_CARGO), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.expect("move master to cargo");
    assert!(stopped(&master));
    assert!(rows(db, ship).await.is_empty());
    assert_eq!(all_bank_pushes(client, rx), vec![vec![Value::Int(ship), Value::Dict(vec![])]]);
    for id in guns { state().active.remove(id); }
}
