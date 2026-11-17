use crate::dao::{items::{creation, repairable_ship as custody, service_life, transfers}, repairable_ship as recovery};
use crate::services::drones::tests::fixture::Fixture;
use infinity_core::consts::{FILETIME_DAY, FILETIME_SECOND, FLAG_CARGO, FLAG_FITTING_FIRST, FLAG_HANGAR, REF_REHABILITATION_COSTS};
use marshal::Value;

async fn rpc(c: &mut crate::client::Client, method: &str, args: &[Value]) -> Value {
    let kwargs = Value::None;
    let call = crate::services::Call { service: "repairableShipMgr", method, args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(c, &call).await.unwrap()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn real_loss_purchase_and_first_withdrawal() {
    let mut f = Fixture::real_system().await;
    let mut extra_system = None;
    let result = crate::tests::catch_scenario(async {
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.client.notify_tx = sender.clone();
        f.world.register_pilot(f.owner, sender);
        let (db, world, owner, ship, system) = (f.db.clone(), f.world.clone(), f.owner, f.ship, f.system);
        let module_type = f.sde.types.iter().find(|t| t.published == Some(true)
            && t.group_id.zip(t.category_id).is_some_and(|(g, c)| f.sde.repairable_ship.enters_recovery(t.type_id, g, c)
                && !f.sde.repairable_ship.unrepairable(c, g, t.type_id))
            && t.base_price.is_some_and(|price| price >= 1_000_000.0)
            && f.sde.repairable_ship.component(t.type_id).is_some()
            && t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)).unwrap().type_id;
        let component = f.sde.repairable_ship.component(module_type).unwrap();
        let module = creation::rookie_ship(&db, module_type, owner, ship, FLAG_FITTING_FIRST).await.unwrap();
        let abort = creation::rookie_ship(&db, module_type, owner, ship, FLAG_FITTING_FIRST + 1).await.unwrap();
        let donor = transfers::insert(&db, component, owner, ship, FLAG_CARGO, 2).await.unwrap();
        db.execute("UPDATE game.items SET name='recovery fixture',custom_info='recovery metadata' WHERE item_id=ANY($1)", &[&&[ship, module][..]]).await.unwrap();
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value,updated_at,cargo_unlimited) VALUES ($1,$2,$3,$4,true)", &[&module, &infinity_core::consts::dogma::ATTR_CAPACITY, &12.5_f64, &7_i64]).await.unwrap();
        db.execute("INSERT INTO game.dynamic_items (item_id,source_item_id,mutator_item_id,source_type_id,mutator_type_id,result_type_id,creator_id,created_at) VALUES ($1,$2,$2,$3,$3,$3,$4,$5)", &[&module, &donor, &module_type, &owner, &1_i64]).await.unwrap();
        db.execute("INSERT INTO game.dynamic_item_modifiers (item_id,attribute_id,factor) VALUES ($1,$2,$3)", &[&module, &infinity_core::consts::dogma::ATTR_CAPACITY, &1.5_f64]).await.unwrap();
        db.execute("INSERT INTO game.ship_configuration (ship_item_id,owner_id) VALUES ($1,$2)", &[&ship, &owner]).await.unwrap();
        db.execute("INSERT INTO game.ship_dirt (ship_item_id,dirt_timestamp) VALUES ($1,$2)", &[&ship, &7_i64]).await.unwrap();
        db.execute("INSERT INTO game.ship_kill_counts (ship_item_id,player_kills) VALUES ($1,$2)", &[&ship, &3_i64]).await.unwrap();
        let ship_type = world.ball(system, ship).await.unwrap().type_id;
        let skin = f.sde.ship_skins.ordinary_skins("optic").find(|skin| skin.type_ids.contains(&ship_type)).unwrap().skin_id;
        db.execute("INSERT INTO game.ship_skin_licenses (character_id,skin_id) VALUES ($1,$2)", &[&owner, &skin]).await.unwrap();
        db.execute("INSERT INTO game.ship_skin_applications (ship_item_id,character_id,skin_id) VALUES ($1,$2,$3)", &[&ship, &owner, &skin]).await.unwrap();
        db.execute("UPDATE game.characters SET balance=1000000000000 WHERE character_id=$1", &[&owner]).await.unwrap();
        let gate = world.lock_condition_snapshot(system).await.unwrap();
        let generation = crate::space::targeting::next_generation();
        let facts = world.with(system, move |s| s.prepare_combat_source(ship, None, None, Some(module_type),
            crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true)).await.unwrap().unwrap();
        let (started, entered) = tokio::sync::oneshot::channel();
        let task_db = db.clone();
        let task_world = world.clone();
        let hit = tokio::spawn(async move {
            crate::services::combat::damage::apply_damage_guarded(&task_db, &task_world, system, ship, [1e30; 4],
                crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false,
                move |_| { let _ = started.send(()); true }).await
        });
        entered.await.unwrap();
        let snapshot = world.with(system, move |s| s.dying_snapshot(ship).unwrap()).await.unwrap();
        assert_eq!((snapshot.item_id, snapshot.identity, snapshot.owner_id), (ship, Some(owner), owner));
        assert!(snapshot.token > 0 && snapshot.generation > 0 && snapshot.lethal_at > 0);
        drop(gate);
        let outcome = hit.await.unwrap().unwrap().unwrap();
        assert!(outcome.lethal && outcome.dealt > 0.0);
        let selection = recovery::list_owned(&db, owner).await.unwrap().into_iter().find(|s| s.group.shape.item.get() == ship).unwrap();
        assert_eq!((selection.group.occurred_at, selection.group.system, selection.group.loss_id), (snapshot.lethal_at, system, Some(ship)));
        assert_eq!(selection.group.payment_expires_at, snapshot.lethal_at + crate::config::game().repairable_ship.payment_retention_days * FILETIME_DAY);
        for member in &selection.items {
            let seconds = f.sde.repairable_ship.repair_seconds(member.type_id).unwrap();
            assert_eq!(member.repair_completed_at, snapshot.lethal_at + crate::client::filetime_ticks_from_ms(crate::space::seconds_to_ms_f64(seconds)));
        }
        let member = |id| selection.items.iter().find(|m| m.item.get() == id).unwrap().clone();
        let (root, module, abort) = (member(ship), member(module), member(abort));
        assert!(selection.items.iter().all(|m| m.item.get() != donor));
        let loss_notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let added = loss_notices.iter().find(|(name, _)| name == "OnAddRecoveryGroup").unwrap();
        let descriptor = crate::gateway::message::message("eve.repairableship.event.ExplodedResponsed").unwrap();
        let actual = prost_reflect::DynamicMessage::parse_text_format(descriptor.clone(), &added.1[0].as_text().unwrap()).unwrap();
        let expected = super::wire::exploded(owner, &selection).unwrap();
        assert_eq!(actual, prost_reflect::DynamicMessage::parse_text_format(descriptor, &expected).unwrap());
        let damages = service_life::get_many(&db, &[root.item, module.item, abort.item]).await.unwrap();
        let life_notice = loss_notices.iter().find(|(name, _)| name == "OnItemServiceLifeDamageChanged").unwrap();
        assert_eq!(life_notice.1[0], super::service_life::damage_dict(&damages));
        assert!(damages[&module.item] > 0.0 && damages[&root.item] > 0.0);
        assert_eq!(rpc(&mut f.client, "GetServiceLifeDamage", &[Value::Int(module.item.get())]).await, Value::Float(damages[&module.item]));
        assert_eq!(rpc(&mut f.client, "CallRequested", &[]).await, Value::None);
        let packet = f.client.outbox.iter().find(|packet| matches!(&packet.destination, infinity_wire::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnRepairableShipResponsed"))).unwrap();
        let payload = marshal::loads(packet.body[0].items().unwrap()[1].as_bytes().unwrap()).unwrap();
        let listed = payload.items().unwrap()[1].items().unwrap();
        let descriptor = crate::gateway::message::message("eve.repairableship.Responsed").unwrap();
        let expected = super::wire::responsed(owner, std::slice::from_ref(&selection)).unwrap();
        assert_eq!(prost_reflect::DynamicMessage::parse_text_format(descriptor.clone(), &listed[0].as_text().unwrap()).unwrap(), prost_reflect::DynamicMessage::parse_text_format(descriptor, &expected).unwrap());
        let group = selection.group.id;
        let station = selection.group.destination_station;
        let destination = Value::Dict(vec![(Value::Int(group.get()), Value::tuple([Value::Int(station), Value::None]))]);
        let expected = Value::Dict(vec![(Value::Int(group.get()), Value::tuple([Value::Int(f.sde.map.station(station).unwrap().solar_system_id.unwrap()), Value::Int(station)]))]);
        assert_eq!(rpc(&mut f.client, "GetBatchDestinationSolarSystem", &[destination]).await, expected);
        let wanted = |ids: &[recovery::RecoveryItemId]| Value::Dict(vec![(Value::Int(group.get()), Value::list(ids.iter().map(|id| Value::Int(id.get()))))]);
        let physical = [root.item, module.item];
        let before_custody = custody::capture_many(&db, &physical).await.unwrap();
        let condition_ids = [infinity_core::consts::dogma::ATTR_SHIELD_CHARGE, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, infinity_core::consts::dogma::ATTR_HULL_DAMAGE];
        let condition = &snapshot.condition;
        let late = [(ship, [condition.now[0], condition.stats.max[1] - condition.now[1], condition.stats.max[2] - condition.now[2]], condition.shield_at)];
        assert!(crate::dao::items::dogma::save_conditions(&db, system, &condition_ids, &late).await.unwrap().is_empty());
        assert_eq!(rpc(&mut f.client, "CallAbortRequested", &[wanted(&[abort.id])]).await, Value::None);
        assert_eq!(recovery::get_member(&db, abort.id).await.unwrap().unwrap().status, recovery::Status::Abandoned);
        assert!(custody::capture_many(&db, &[abort.item]).await.unwrap().is_empty());
        assert_eq!(custody::capture_many(&db, &physical).await.unwrap(), before_custody);
        assert!(db.query_opt("SELECT 1 FROM game.jobs WHERE job_id=$1", &[&super::deadlines::job_id(super::deadlines::PAYMENT, abort.id)]).await.unwrap().is_none());
        let balance = crate::dao::wallets::characters::balance(&db, owner).await.unwrap();
        let config = &f.sde.repairable_ship;
        let mut cost = crate::isk::IskAmount::ZERO;
        let mut member_costs = std::collections::BTreeMap::new();
        for member in [&root, &module] {
            let base = f.sde.types.get(member.type_id).unwrap().base_price.unwrap_or(0.0);
            let market = f.sde.prices.get(member.type_id).map(|price| price.average).unwrap_or(0.0);
            let share = (member.last_service_life - (config.max_life() - damages[&member.item])).abs() / config.max_life();
            let amount = crate::isk::IskAmount::from_whole_isk((base.max(market) * member.quantity as f64 * config.price_percentage() * share).trunc() as i128).unwrap();
            member_costs.insert(member.id, amount);
            cost = cost.checked_add(amount).unwrap();
        }
        assert_eq!(rpc(&mut f.client, "CallDeliveryPurchased", &[wanted(&[root.id, module.id])]).await, Value::None);
        let after_payment = crate::dao::wallets::characters::balance(&db, owner).await.unwrap();
        assert_eq!(balance.checked_sub(after_payment).unwrap(), cost);
        let journals = db.query("SELECT amount::text AS amount FROM game.wallet_journal WHERE owner_id1=$1 AND entry_type_id=$2 AND reference_id=$3", &[&owner, &REF_REHABILITATION_COSTS, &group.get()]).await.unwrap();
        assert_eq!(journals.len(), 1);
        assert_eq!(crate::isk::IskAmount::parse_decimal(journals[0].get::<_, String>("amount").as_str()).unwrap(), cost.checked_neg().unwrap());
        let ids = [root.id.get(), module.id.get()];
        let receipt_sql = "SELECT member_id,paid_at,paid_amount::text AS amount,delivery_completed_at,withdraw_expires_at,finished_at FROM game.repairable_recovery_items WHERE member_id=ANY($1) ORDER BY member_id";
        let receipt = |row: &tokio_postgres::Row| (row.get::<_, i64>("member_id"), row.get::<_, Option<i64>>("paid_at"), row.get::<_, Option<String>>("amount"), row.get::<_, Option<i64>>("delivery_completed_at"), row.get::<_, Option<i64>>("withdraw_expires_at"), row.get::<_, Option<i64>>("finished_at"));
        let paid = db.query(receipt_sql, &[&ids.as_slice()]).await.unwrap();
        let before_receipt: Vec<_> = paid.iter().map(receipt).collect();
        for member in [&root, &module] {
            let row = paid.iter().find(|row| row.get::<_, i64>("member_id") == member.id.get()).unwrap();
            assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("amount")).unwrap(), member_costs[&member.id]);
            let paid_at: i64 = row.get("paid_at");
            let delivered: i64 = row.get("delivery_completed_at");
            let expires: i64 = row.get("withdraw_expires_at");
            assert_eq!(delivered, paid_at.max(member.repair_completed_at) + crate::config::game().repairable_ship.delivery_seconds * FILETIME_SECOND);
            assert_eq!(expires, delivered + crate::config::game().repairable_ship.withdrawal_retention_days * FILETIME_DAY);
            assert_eq!(recovery::get_member(&db, member.id).await.unwrap().unwrap().status, recovery::Status::Delivering);
            assert!(db.query_opt("SELECT 1 FROM game.jobs WHERE job_id=$1", &[&super::deadlines::job_id(super::deadlines::PAYMENT, member.id)]).await.unwrap().is_none());
            for job in [super::deadlines::delivery_job(member.id, delivered), super::deadlines::withdraw_job(member.id, expires)] {
                assert_eq!(db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&job.id]).await.unwrap().get::<_, i64>("due_at"), job.due_at);
            }
        }
        assert_eq!(rpc(&mut f.client, "CallDeliveryPurchased", &[wanted(&[root.id, module.id])]).await, Value::None);
        assert_eq!(db.query(receipt_sql, &[&ids.as_slice()]).await.unwrap().iter().map(receipt).collect::<Vec<_>>(), before_receipt);
        assert_eq!(crate::dao::wallets::characters::balance(&db, owner).await.unwrap(), after_payment);
        assert_eq!(db.query("SELECT 1 FROM game.wallet_journal WHERE owner_id1=$1 AND entry_type_id=$2 AND reference_id=$3", &[&owner, &REF_REHABILITATION_COSTS, &group.get()]).await.unwrap().len(), 1);
        let delivered = crate::client::filetime_now() - 2;
        db.execute("UPDATE game.repairable_recovery_items SET repair_completed_at=$2,paid_at=$2,delivery_completed_at=$3,withdraw_expires_at=$4 WHERE member_id=ANY($1)", &[&ids.as_slice(), &(delivered - 1), &delivered, &(delivered + FILETIME_DAY)]).await.unwrap();
        while rx.try_recv().is_ok() {}
        assert_eq!(rpc(&mut f.client, "CallItemsWithdrawalRequested", &[wanted(&[root.id, module.id])]).await, Value::None);
        let root_after = recovery::get_member(&db, root.id).await.unwrap().unwrap();
        let module_after = recovery::get_member(&db, module.id).await.unwrap().unwrap();
        assert_eq!((root_after.status, module_after.status), (recovery::Status::Withdrawn, recovery::Status::Withdrawn));
        let (new_root, new_module) = (root_after.withdrawn_item.unwrap(), module_after.withdrawn_item.unwrap());
        assert!(new_root != root.item && new_module != module.item && new_root != new_module);
        assert!(custody::capture_many(&db, &physical).await.unwrap().is_empty());
        let restored = custody::capture_many(&db, &[new_root, new_module]).await.unwrap();
        assert_eq!(restored.len(), 2);
        assert!(restored.iter().all(|item| item.owner == owner && item.location == station && item.flag == FLAG_HANGAR));
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(notices.first().unwrap().0, "OnItemChange");
        assert_eq!(notices.last().unwrap().0, "OnItemsWithdrawn");
        let withdrawn = recovery::Selection { group: selection.group.clone(), items: vec![root_after, module_after] };
        let descriptor = crate::gateway::message::message("eve.repairableship.ItemsWithdrawn").unwrap();
        let expected = super::wire::items_withdrawn(owner, &[withdrawn]).unwrap();
        assert_eq!(prost_reflect::DynamicMessage::parse_text_format(descriptor.clone(), &notices.last().unwrap().1[0].as_text().unwrap()).unwrap(), prost_reflect::DynamicMessage::parse_text_format(descriptor, &expected).unwrap());
        for id in [new_root.get(), new_module.get()] {
            let row = db.query_one("SELECT name,custom_info,x,y,z FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
            assert_eq!((row.get::<_, Option<String>>("name").as_deref(), row.get::<_, Option<String>>("custom_info").as_deref()), (Some("recovery fixture"), Some("recovery metadata")));
            assert_eq!((row.get::<_, Option<f64>>("x"), row.get::<_, Option<f64>>("y"), row.get::<_, Option<f64>>("z")), (None, None, None));
        }
        let attr = db.query_one("SELECT value,updated_at,cargo_unlimited FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2", &[&new_module.get(), &infinity_core::consts::dogma::ATTR_CAPACITY]).await.unwrap();
        assert_eq!((attr.get::<_, f64>("value"), attr.get::<_, i64>("updated_at"), attr.get::<_, bool>("cargo_unlimited")), (12.5, 7, true));
        let dynamic = db.query_one("SELECT source_item_id,mutator_item_id,source_type_id,mutator_type_id,result_type_id,creator_id,created_at FROM game.dynamic_items WHERE item_id=$1", &[&new_module.get()]).await.unwrap();
        assert_eq!((dynamic.get::<_, i64>("source_item_id"), dynamic.get::<_, i64>("mutator_item_id"), dynamic.get::<_, i32>("source_type_id"), dynamic.get::<_, i32>("mutator_type_id"), dynamic.get::<_, i32>("result_type_id"), dynamic.get::<_, i64>("creator_id"), dynamic.get::<_, i64>("created_at")), (donor, donor, module_type, module_type, module_type, owner, 1));
        let modifier = db.query_one("SELECT factor FROM game.dynamic_item_modifiers WHERE item_id=$1 AND attribute_id=$2", &[&new_module.get(), &infinity_core::consts::dogma::ATTR_CAPACITY]).await.unwrap();
        assert_eq!(modifier.get::<_, f64>("factor"), 1.5);
        let hull = db.query_one("SELECT c.owner_id,c.fleet_hangar_fleet,c.fleet_hangar_corp,c.maintenance_fleet,c.maintenance_corp,d.dirt_timestamp,k.player_kills,a.skin_id FROM game.ship_configuration c JOIN game.ship_dirt d USING (ship_item_id) JOIN game.ship_kill_counts k USING (ship_item_id) JOIN game.ship_skin_applications a USING (ship_item_id) WHERE c.ship_item_id=$1", &[&new_root.get()]).await.unwrap();
        assert_eq!((hull.get::<_, i64>("owner_id"), hull.get::<_, bool>("fleet_hangar_fleet"), hull.get::<_, bool>("fleet_hangar_corp"), hull.get::<_, bool>("maintenance_fleet"), hull.get::<_, bool>("maintenance_corp")), (owner, false, false, false, false));
        assert_eq!((hull.get::<_, i64>("dirt_timestamp"), hull.get::<_, i64>("player_kills"), hull.get::<_, i32>("skin_id")), (7, 3, skin));
        let new_ids = [new_root.get(), new_module.get()];
        assert!(db.query("SELECT 1 FROM game.item_attributes WHERE item_id=ANY($1) AND attribute_id=ANY($2)", &[&new_ids.as_slice(), &condition_ids.as_slice()]).await.unwrap().is_empty());
        assert!(custody::online(&db, &[new_root, new_module]).await.unwrap().is_empty());
        for table in ["module_charges", "weapon_banks", "pending_reloads"] {
            let sql = format!("SELECT 1 FROM game.{table} WHERE ship_id=ANY($1)");
            assert!(db.query(&sql, &[&new_ids.as_slice()]).await.unwrap().is_empty());
        }
        assert!(db.query("SELECT 1 FROM game.insurance WHERE ship_id=ANY($1)", &[&new_ids.as_slice()]).await.unwrap().is_empty());
        assert_eq!(service_life::get_many(&db, &[new_root, new_module]).await.unwrap(), [(new_root, damages[&root.item]), (new_module, damages[&module.item])].into_iter().collect());
        assert!(crate::dao::items::dogma::save_conditions(&db, system, &condition_ids, &late).await.unwrap().is_empty());
        assert_eq!(custody::capture_many(&db, &[new_root, new_module]).await.unwrap(), restored);
        let before_repeat: Vec<_> = db.query(receipt_sql, &[&ids.as_slice()]).await.unwrap().iter().map(receipt).collect();
        let inventory_count: i64 = db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND location_id=$2", &[&owner, &station]).await.unwrap().get("n");
        assert_eq!(rpc(&mut f.client, "CallItemsWithdrawalRequested", &[wanted(&[root.id, module.id])]).await, Value::None);
        assert_eq!(rx.try_recv().unwrap().0, "OnItemsWithdrawn");
        assert!(rx.try_recv().is_err());
        assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND location_id=$2", &[&owner, &station]).await.unwrap().get::<_, i64>("n"), inventory_count);
        assert_eq!(db.query(receipt_sql, &[&ids.as_slice()]).await.unwrap().iter().map(receipt).collect::<Vec<_>>(), before_repeat);
        assert_eq!(crate::dao::wallets::characters::balance(&db, owner).await.unwrap(), after_payment);
        for (member, item) in [(&root, new_root), (&module, new_module)] {
            let historical = recovery::get_member(&db, member.id).await.unwrap().unwrap();
            assert_eq!((historical.status, historical.withdrawn_item), (recovery::Status::Withdrawn, Some(item)));
            let jobs = [super::deadlines::job_id(super::deadlines::DELIVERY, member.id), super::deadlines::job_id(super::deadlines::WITHDRAW, member.id)];
            assert!(db.query("SELECT 1 FROM game.jobs WHERE job_id=ANY($1)", &[&jobs.as_slice()]).await.unwrap().is_empty());
        }
        assert_eq!(recovery::get_member(&db, abort.id).await.unwrap().unwrap().status, recovery::Status::Abandoned);
        assert_eq!(rpc(&mut f.client, "GetServiceLifeDamage", &[Value::Int(new_module.get())]).await, Value::Float(damages[&module.item]));
        let destination_station = f.sde.map.station(station).unwrap();
        let (destination_system, constellation, region) = (destination_station.solar_system_id.unwrap(), destination_station.constellation_id.unwrap(), destination_station.region_id.unwrap());
        let capsule = crate::dao::characters::selection::ship_id(&db, owner).await.unwrap().unwrap();
        if world.ball(system, capsule).await.is_some() { world.remove_ball(system, capsule).await; }
        db.execute("UPDATE game.items SET location_id=$2,flag_id=$3,x=NULL,y=NULL,z=NULL WHERE item_id=$1 AND owner_id=$4", &[&capsule, &station, &FLAG_HANGAR, &owner]).await.unwrap();
        db.execute("UPDATE game.characters SET station_id=$2,solar_system_id=$3,constellation_id=$4,region_id=$5 WHERE character_id=$1 AND ship_id=$6", &[&owner, &station, &destination_system, &constellation, &region, &capsule]).await.unwrap();
        f.client.session.apply(vec![("shipid", Value::Int(capsule)), ("stationid", Value::Int(station)), ("solarsystemid", Value::None), ("solarsystemid2", Value::Int(destination_system)), ("constellationid", Value::Int(constellation)), ("regionid", Value::Int(region))]);
        let inventory = Value::tuple([Value::Int(station)]);
        let global = Value::tuple([Value::Int(owner), Value::Int(10002)]);
        for (service, flag, binding) in [("invInventory", Value::Int(i64::from(FLAG_HANGAR)), &inventory), ("charMgr", Value::None, &global)] {
            let args = [flag];
            let kwargs = Value::None;
            let call = crate::services::Call { service, method: "ListServiceLifeDamage", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(binding) };
            let Value::Dict(rows) = crate::services::dispatch(&mut f.client, &call).await.unwrap() else { panic!("life map") };
            assert!(rows.contains(&(Value::Int(new_module.get()), Value::Float(damages[&module.item]))));
        }
        let workshop_donor = transfers::insert(&db, module_type, owner, station, FLAG_HANGAR, 2).await.unwrap();
        let args = [Value::list([Value::Int(workshop_donor)]), Value::Int(station), Value::Int(owner), Value::Int(station), Value::Int(i64::from(FLAG_HANGAR))];
        assert_eq!(rpc(&mut f.client, "OnDisassemble", &args).await, Value::list([Value::Int(workshop_donor)]));
        use crate::dao::items::loading::{load_items, ItemQuery};
        assert!(load_items(&db, ItemQuery::Id(workshop_donor)).await.unwrap().is_empty());
        let before_components: i64 = load_items(&db, ItemQuery::LocationFlagTypeOwnerStack(station, FLAG_HANGAR, component, owner)).await.unwrap().iter().map(|item| item.quantity).sum();
        assert!(before_components > 0);
        let row = crate::services::items::load_items(&db, &f.sde, ItemQuery::Id(new_module.get())).await.unwrap().remove(0).row();
        let remaining = (damages[&module.item] - f.sde.repairable_ship.restore_per_component()).max(0.0);
        let args = [Value::list([row.clone()]), Value::Dict(vec![(Value::Int(i64::from(component)), Value::Int(1))])];
        while rx.try_recv().is_ok() {}
        assert_eq!(rpc(&mut f.client, "OnRefurbishment", &args).await, Value::Dict(vec![(row, Value::Float(remaining))]));
        assert_eq!(service_life::get_many(&db, &[new_module]).await.unwrap()[&new_module], remaining);
        let after_components: i64 = load_items(&db, ItemQuery::LocationFlagTypeOwnerStack(station, FLAG_HANGAR, component, owner)).await.unwrap().iter().map(|item| item.quantity).sum();
        assert_eq!(after_components, before_components - 1);
        let life_notice = std::iter::from_fn(|| rx.try_recv().ok()).find(|(name, _)| name == "OnItemServiceLifeDamageChanged").unwrap().1;
        assert_eq!(life_notice, vec![Value::Dict(vec![(Value::Int(new_module.get()), Value::Float(remaining))])]);
        assert_eq!(crate::dao::wallets::characters::balance(&db, owner).await.unwrap(), after_payment);
        let hull_component = f.sde.repairable_ship.component(ship_type).expect("hull must have a refurbishment component");
        let hull_donor = transfers::insert(&db, ship_type, owner, station, FLAG_HANGAR, 1).await.unwrap();
        let args = [Value::list([Value::Int(hull_donor)]), Value::Int(station), Value::Int(owner), Value::Int(station), Value::Int(i64::from(FLAG_HANGAR))];
        assert_eq!(rpc(&mut f.client, "OnDisassemble", &args).await, Value::list([Value::Int(hull_donor)]));
        let count = (((damages[&root.item] + config.max_reduce() - config.max_life()) / config.restore_per_component()).floor() as i64 + 1).max(1);
        let hull_row = crate::services::items::load_items(&db, &f.sde, ItemQuery::Id(new_root.get())).await.unwrap().remove(0).row();
        let args = [Value::list([hull_row.clone()]), Value::Dict(vec![(Value::Int(i64::from(hull_component)), Value::Int(count))])];
        let remaining_hull = (damages[&root.item] - count as f64 * config.restore_per_component()).max(0.0);
        assert_eq!(rpc(&mut f.client, "OnRefurbishment", &args).await, Value::Dict(vec![(hull_row, Value::Float(remaining_hull))]));
        let hull_damage = service_life::get_many(&db, &[new_root]).await.unwrap()[&new_root];
        assert!(hull_damage + config.max_reduce() < config.max_life(), "refurbished hull must remain recoverable after the maximum next loss");
        let id = new_root.get();
        crate::dao::items::ship::undock(&db, id, destination_system, [0.0; 3]).await.unwrap();
        crate::dao::characters::selection::set_ship(&db, owner, id).await.unwrap();
        crate::dao::characters::ship::undock(&db, owner, destination_system).await.unwrap();
        f.client.session.apply(vec![("shipid", Value::Int(id)), ("stationid", Value::None), ("solarsystemid", Value::Int(destination_system)), ("solarsystemid2", Value::Int(destination_system))]);
        extra_system = Some(destination_system);
        world.system(destination_system).await.unwrap();
        let row = crate::dao::items::space_ships::FreeShip { item_id: id, type_id: ship_type, owner_id: owner, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = crate::services::ship::balls::piloted(&db, f.sde.clone(), &row, owner, 0.0).await.unwrap();
        world.with(destination_system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let generation = crate::space::targeting::next_generation();
        let facts = world.with(destination_system, move |s| s.prepare_combat_source(id, None, None, Some(module_type), crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true)).await.unwrap().unwrap();
        let outcome = crate::services::combat::damage::apply_damage_guarded(&db, &world, destination_system, id, [1e30; 4], crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false, |_| true).await.unwrap().unwrap();
        assert!(outcome.lethal && outcome.dealt > 0.0);
        let second = recovery::list_owned(&db, owner).await.unwrap().into_iter().find(|s| s.group.shape.item == new_root).unwrap();
        assert_ne!(second.group.id, group);
        assert_eq!(second.group.loss_id, Some(id));
        let key = crate::db::outbox::SourceKey::canonical("LossSettled", id, 0);
        let new_source = crate::dao::progress::source(&db, "business", key.as_str()).await.unwrap().unwrap().event_id;
        let kills = db.query("SELECT victim_item_id FROM game.killmails WHERE victim_item_id=ANY($1) ORDER BY victim_item_id", &[&&[ship, id][..]]).await.unwrap();
        assert_eq!(kills.iter().map(|k| k.get::<_, i64>("victim_item_id")).collect::<Vec<_>>(), [ship.min(id), ship.max(id)]);
        let second_custody = custody::capture_many(&db, &[new_root]).await.unwrap();
        crate::services::combat::damage::reconcile_pending_loss(db.clone(), world.clone(), system, ship, snapshot.clone()).await;
        assert_eq!(custody::capture_many(&db, &[new_root]).await.unwrap(), second_custody);
        assert_eq!(recovery::get_group(&db, group).await.unwrap().unwrap().shape.item, root.item);
        assert_eq!(recovery::get_group(&db, second.group.id).await.unwrap().unwrap().shape.item, new_root);
        assert_eq!(crate::dao::progress::source(&db, "business", key.as_str()).await.unwrap().unwrap().event_id, new_source);
    }).await;
    if let Some(system) = extra_system {
        if let Some(id) = f.world.with(system, { let owner = f.owner; move |s| s.pilot_ball(owner).map(|ball| ball.id) }).await.flatten() { f.world.remove_ball(system, id).await; }
    }
    f.db.execute("DELETE FROM game.wallet_journal WHERE owner_id1=$1 OR owner_id2=$1", &[&f.owner]).await.unwrap();
    f.db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&f.owner]).await.unwrap();
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn dispatcher_rejects_unselected_character() {
    let mut f = Fixture::real_system().await;
    let result = crate::tests::catch_scenario(async {
        f.client.session.apply(vec![("charid", Value::None)]);
        f.client.outbox.clear();
        let kwargs = Value::None;
        let call = crate::services::Call { service: "repairableShipMgr", method: "CallRequested", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let result = crate::services::dispatch(&mut f.client, &call).await;
        assert!(matches!(result, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::repairable_ship::SELECTION_INVALID))]));
        assert!(f.client.outbox.is_empty());
    }).await;
    f.client.session.apply(vec![("charid", Value::Int(f.owner))]);
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn dispatcher_rejects_invalid_service_life_argument() {
    let mut f = Fixture::real_system().await;
    let result = crate::tests::catch_scenario(async {
        let kwargs = Value::None;
        let args = [Value::str("invalid")];
        let call = crate::services::Call { service: "repairableShipMgr", method: "GetServiceLifeDamage", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let result = crate::services::dispatch(&mut f.client, &call).await;
        assert!(matches!(result, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::repairable_ship::SELECTION_INVALID))]));
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
