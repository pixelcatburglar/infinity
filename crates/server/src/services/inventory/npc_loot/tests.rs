//! Real-client coverage for transient NPC loot through the public inventory
//! RPCs and shared movement transaction, plus the one shared PostgreSQL gate
//! support the effects tests also consume. Every case runs a real `World`,
//! `Db` and SDE fixture; nothing here fabricates claim states.

use std::future::Future;
use std::pin::Pin;

const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

pub(in crate::services) mod support;

#[cfg(feature = "tests-inventory")]
mod inventory {
    use super::*;
    use super::support::{control_client, drive_gated, finish_proof, named_db, report_cleanup_failures, wait_driver};
    use super::super::*;
    use crate::client::Client;
    use crate::db::Db;
    use crate::sde::Sde;
    use infinity_core::consts::{CATEGORY_MODULE, FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_FITTING_FIRST, FLAG_HIDDEN_MODIFIERS};
    use crate::services::{Call, dispatch, tests::fixture};
    use crate::space::{Ball, WarpState, MODE_STOP, V3};
    use crate::lua::events::GameEvent;
    use crate::lua::events::resources::{FactContext, FactOrigin};
    use crate::space::system::npc_wrecks::RecoveryAttempt;

    fn ball(id: i64, type_id: i32, owner: i64, character: Option<i64>, pos: V3) -> Ball {
        Ball { id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: character,
            signature_radius: None, mode: MODE_STOP, radius: 50.0, mass: 1.0, max_vel: 0.0, agility: 1.0,
            speed_fraction: 0.0, pos, vel: [0.0; 3], old_pos: pos, old_vel: [0.0; 3],
            segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
            free: false, global: false, massive: false, interactive: false,
            warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
            cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
    }

    struct Space {
        db: Db, sde: Arc<Sde>, world: Arc<World>, fixture: fixture::Fixture,
        client: Client, _peer: tokio::net::TcpStream,
        pool_name: String,
        system: i64, ship: i64, ship_type: i32, wreck_type: i32, loot_type: i32,
    }

    async fn space_at(system: i64) -> Space {
        crate::tests::init_config();
        let pool_name = format!("npc_test_p_{}", crate::handshake::random_u64());
        let db = named_db(&pool_name).await;
        let sde = crate::tests::shared_sde(&db).await;
        let f = fixture::create(&db).await.unwrap();
        let (mut c, peer) = crate::services::tests::client(db.clone(), f.character).await;
        let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
        let ship = crate::dao::items::creation::positioned(&db, ship_type, f.character, system, [0.0; 3]).await.unwrap();
        crate::dao::characters::selection::set_ship(&db, f.character, ship).await.unwrap();
        db.execute("UPDATE game.characters SET solar_system_id=$2, ship_id=$3, station_id=NULL WHERE character_id=$1",
            &[&f.character, &system, &ship]).await.unwrap();
        c.session.apply(vec![("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        c.world.add_ship(system, ball(ship, ship_type, f.character, Some(f.character), [0.0; 3])).await.unwrap();
        let wreck_type = sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_WRECK)
            && !sde.typelists.is_untrashable(t.type_id, infinity_core::consts::GROUP_WRECK, t.category_id.unwrap_or_default())).unwrap().type_id;
        let loot_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
            && t.published == Some(true) && t.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
        Space { db, sde, world: c.world.clone(), fixture: f, client: c, _peer: peer,
            system, ship, ship_type, wreck_type, loot_type, pool_name }
    }

    async fn space() -> Space { space_at(1_000_000_000_000_i64 + crate::handshake::random_u64() as i64 % 1_000_000).await }

    /// Spawn a transient wreck root with explicit loot; returns (root, loot ids).
    async fn spawn(f: &Space, owner: i64, pos: V3, loot: Vec<(i32, i64)>) -> (i64, Vec<i64>) {
        let radius = f.sde.types.get(f.wreck_type).and_then(|t| t.radius).unwrap_or(100.0) as f32;
        let (wreck_type, system) = (f.wreck_type, f.system);
        let root = f.world.with(system, move |s|
            s.npc_wreck_spawn(wreck_type, owner, 1, pos, radius, None, loot)).await.unwrap().expect("reserved ids");
        let items = f.world.with(system, move |s| match s.npc_wreck_recovery_view(
            &NpcWreckAccess::GmCleanup { character: 1 }, root) {
            NpcWreckLookup::Ready(view) => view.items.iter().map(|i| i.item_id).collect(),
            _ => panic!("spawned root must read"),
        }).await.unwrap();
        (root, items)
    }

    /// Live (id, quantity, owner, flag) rows in a transient root; `None` when gone.
    async fn view(f: &Space, root: i64) -> Option<Vec<(i64, i64, i64, i32)>> {
        f.world.with(f.system, move |s| match s.npc_wreck_recovery_view(
            &NpcWreckAccess::GmCleanup { character: 1 }, root) {
            NpcWreckLookup::Ready(view) => Some(view.items.iter().map(|i| (i.item_id, i.quantity, i.owner_id, i.flag_id)).collect()),
            NpcWreckLookup::Absent => None,
            _ => panic!("unexpected lookup"),
        }).await.unwrap()
    }

    /// The registered claim Arc while a root is busy, for proof assertions.
    async fn busy_claim(world: &Arc<World>, system: i64, root: i64) -> Option<Arc<NpcWreckClaim>> {
        world.with(system, move |s| match s.npc_wreck_recovery_view(
            &NpcWreckAccess::GmCleanup { character: 1 }, root) {
            NpcWreckLookup::Busy(claim) => claim,
            _ => None,
        }).await.unwrap()
    }

    async fn bound_params(client: &mut Client, method: &str, args: &[Value]) -> Value {
        let kwargs = Value::Dict(vec![]);
        let bound = dispatch(client, &Call { service: "invbroker", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await.unwrap();
        let Value::Callback(callback) = bound else { panic!("expected bound moniker") };
        let oid = marshal::loads(callback.as_bytes().unwrap()).unwrap().items().unwrap()[0].as_text().unwrap().into_owned();
        client.bound.get(&oid).expect("registered bind").1.clone()
    }

    /// A bound call on an already-registered moniker.
    async fn call(client: &mut Client, method: &str, params: &Value, args: &[Value], kwargs: &Value) -> Result<Value, CallError> {
        dispatch(client, &Call { service: crate::services::inventory::INVENTORY, method, args, kwargs, bound: true, bind_params: Some(params) }).await
    }

    fn field(row: &Value, name: &str) -> Option<Value> { crate::services::item_descriptor().get(row, name) }

    /// A fixture-unique trigger proving no `game.items` write touches fixture
    /// ids during a read; it never claims to prove zero SELECTs or no market.
    async fn db_guard(db: &Db, tag: &str, ids: &[i64]) {
        db_unguard(db, tag).await;
        db.execute(&format!("CREATE OR REPLACE FUNCTION game.{tag}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'transient root materialized'; END $$"), &[]).await.unwrap();
        let clause = ids.iter().map(|id| format!("NEW.item_id={id} OR NEW.location_id={id}")).collect::<Vec<_>>().join(" OR ");
        db.execute(&format!("CREATE TRIGGER {tag} BEFORE INSERT OR UPDATE ON game.items FOR EACH ROW WHEN ({clause}) EXECUTE FUNCTION game.{tag}()"), &[]).await.unwrap();
    }

    async fn db_unguard(db: &Db, tag: &str) {
        db.execute(&format!("DROP TRIGGER IF EXISTS {tag} ON game.items"), &[]).await.unwrap();
        db.execute(&format!("DROP FUNCTION IF EXISTS game.{tag}()"), &[]).await.unwrap();
    }

    /// Wait until the registered claim slot reaches `expected`.
    async fn wait_claim(f: &Space, claim: &Arc<NpcWreckClaim>, expected: Option<NpcWreckClaimState>) {
        let claim = claim.clone();
        tokio::time::timeout(WAIT, async {
            loop {
                let claim = claim.clone();
                if f.world.with(claim.system(), move |s| s.npc_wreck_claim_status(&claim)).await.flatten() == expected { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("claim status never settled");
    }

    #[tokio::test]
    #[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
    async fn transient_reads_memory_mutations_and_owner_rules() {
        let mut f = space().await;
        let npc_owner = 999_999_001_i64; // an NPC corporation absent from game.corporations
        let owner = f.fixture.character;
        let type2 = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
            && t.published == Some(true) && t.type_id != f.loot_type).unwrap().type_id;
        let type3 = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
            && t.published == Some(true) && t.type_id != f.loot_type && t.type_id != type2).unwrap().type_id;
        let charge = f.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
            && t.published == Some(true) && t.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
        let (root_n, npc_ids) = spawn(&f, npc_owner, [50.0, 0.0, 0.0],
            vec![(f.loot_type, 7), (type3, 3), (charge, 2), (charge, 2), (charge, 2)]).await;
        let (root_own, own_ids) = spawn(&f, owner, [60.0, 0.0, 0.0], vec![(f.loot_type, 6), (type2, 4)]).await;
        let (root_own2, own2_ids) = spawn(&f, owner, [70.0, 0.0, 0.0], vec![(f.loot_type, 2)]).await;
        let other_sys = f.system + 1;
        f.world.system(other_sys).await.unwrap();
        let far_root = f.world.with(other_sys, move |s|
            s.npc_wreck_spawn(f.wreck_type, owner, 1, [0.0; 3], 1.0, None, vec![])).await.unwrap().expect("reserved ids");
        let tag = format!("npc_guard_{root_n}");
        let scenario = crate::tests::catch_scenario(async {
            db_guard(&f.db, &tag, &[root_n, root_own, root_own2]).await;
            let none = Value::Dict(vec![]);
            let params = bound_params(&mut f.client, "GetInventoryFromId", &[Value::Int(root_n)]).await;
            let list = call(&mut f.client, "List", &params, &[], &none).await.unwrap();
            let listed: Vec<i64> = list.items().unwrap().iter().filter_map(|row| field(row, "itemID").and_then(|v| v.as_int())).collect();
            assert_eq!(listed, npc_ids, "an NPC-corporation-owned root lists for the pilot");
            assert_eq!(call(&mut f.client, "ListByFlags", &params, &[], &none).await.unwrap().items().unwrap().len(), 5);
            let item = call(&mut f.client, "GetItem", &params, &[], &none).await.unwrap();
            assert_eq!(field(&item, "itemID"), Some(Value::Int(root_n)));
            assert_eq!(field(&item, "quantity"), Some(Value::Int(infinity_core::consts::SINGLETON_QUANTITY)));
            let Value::DbRow { descriptor, .. } = &item else { panic!("inventory row") };
            let descriptor = marshal::DbRowDescriptor::from_value(descriptor).expect("returned row descriptor");
            assert!(descriptor.virtual_columns.iter().any(|(name, function)| name == "singleton" && function == "eve.common.script.sys.eveCfg.Singleton"));
            let capacity = call(&mut f.client, "GetCapacity", &params, &[], &none).await.unwrap();
            let Value::Instance(class, state) = &capacity else { panic!("capacity keyval") };
            assert_eq!(**class, Value::str("utillib.KeyVal"));
            let Value::Dict(_) = state.as_ref() else { panic!("capacity keyval state") };
            assert!(state.get("capacity").and_then(|v| v.as_float()).is_some()
                && state.get("used").and_then(|v| v.as_float()).is_some_and(|u| u > 0.0));
            assert_eq!(call(&mut f.client, "ListSoulBound", &params, &[], &none).await.unwrap(), Value::Dict(vec![]));
            assert_eq!(call(&mut f.client, "ListServiceLifeDamage", &params, &[], &none).await.unwrap(), Value::Dict(vec![]));
            assert_eq!(call(&mut f.client, "GetAvailableTurretSlots", &params, &[], &none).await.unwrap(), Value::list([]));
            // Bound broker reads keep their shape: contents rows and an evelocations row.
            let sys_params = Value::Tuple(vec![Value::Int(f.system), Value::Int(i64::from(infinity_core::consts::GROUP_SOLAR_SYSTEM))]);
            let kwargs = Value::Dict(vec![]);
            let contents = dispatch(&mut f.client, &Call { service: "invbroker", method: "GetContainerContents",
                args: &[Value::Int(root_n), Value::Int(f.system)], kwargs: &kwargs, bound: true, bind_params: Some(&sys_params) }).await.unwrap();
            let contents_ids: Vec<i64> = contents.items().unwrap().iter().filter_map(|row| field(row, "itemID").and_then(|v| v.as_int())).collect();
            assert_eq!(contents_ids, npc_ids);
            let invalid = dispatch(&mut f.client, &Call { service: "invbroker", method: "ValidateItemListCanBeOpened",
                args: &[Value::List(vec![item.clone()])], kwargs: &kwargs, bound: true, bind_params: Some(&sys_params) }).await.unwrap();
            assert_eq!(invalid.items().unwrap().to_vec(), vec![Value::Int(root_n)]);
            let locations = dispatch(&mut f.client, &Call { service: "config", method: "GetLocationsEx",
                args: &[Value::List(vec![Value::Int(root_n)])], kwargs: &Value::None, bound: false, bind_params: None }).await.unwrap();
            let loc = locations.items().unwrap()[1].items().unwrap().iter().find(|row| row.items().unwrap()[0] == Value::Int(root_n)).expect("location row");
            assert_eq!(loc.items().unwrap()[1], Value::unicode(f.sde.types.get(f.wreck_type).and_then(|t| t.name.clone()).unwrap_or_default()));
            // A wreck is never a renameable container through the label path.
            assert!(crate::services::inventory::labels::set_label(&mut f.client, root_n, "renamed".to_string()).await.is_err());
            // The watching second pilot sees every real payload shape below.
            let other = fixture::create(&f.db).await.unwrap();
            let other_ship = crate::dao::items::creation::positioned(&f.db, f.ship_type, other.character, f.system, [65.0, 0.0, 0.0]).await.unwrap();
            f.world.add_ship(f.system, ball(other_ship, f.ship_type, other.character, Some(other.character), [65.0, 0.0, 0.0])).await.unwrap();
            let (notice_tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
            f.world.register_pilot(other.character, notice_tx);
            // Own in-place split and folds: new id, flag kept, no row, quantity conserved.
            let created = dispatch(&mut f.client, &Call { service: "invbroker", method: "SplitStack",
                args: &[Value::Int(root_own), Value::Int(own_ids[0]), Value::Int(2)], kwargs: &kwargs, bound: false, bind_params: None }).await.unwrap();
            let created = created.as_int().expect("split returns the new stack id");
            assert!(f.db.query_opt("SELECT 1 FROM game.items WHERE item_id=$1", &[&created]).await.unwrap().is_none());
            assert!(view(&f, root_own).await.unwrap().iter().any(|&(id, q, o, fl)| id == created && q == 2 && o == owner && fl == 0),
                "a broker SplitStack keeps the source flag");
            assert_eq!(view(&f, root_own).await.unwrap().iter().map(|&(_, q, _, _)| q).sum::<i64>(), 10);
            let wparams = bound_params(&mut f.client, "GetInventoryFromId", &[Value::Int(root_own)]).await;
            call(&mut f.client, "MultiMerge", &wparams,
                &[Value::list([Value::list([Value::Int(created), Value::Int(own_ids[0]), Value::Int(2)])]), Value::Int(root_own)], &kwargs).await.unwrap();
            call(&mut f.client, "StackAll", &wparams, &[], &kwargs).await.unwrap();
            assert_eq!(view(&f, root_own).await.unwrap().len(), 2, "same-type stacks fold");
            // A foreign destination root refuses; the foreign source cannot run
            // same-root whole or partial operations either; a different system
            // actor's transient id is unreachable.
            assert!(call(&mut f.client, "Add", &params, &[Value::Int(own2_ids[0]), Value::Int(root_own2)], &none).await.is_err(), "foreign destination refused");
            assert!(call(&mut f.client, "Add", &params, &[Value::Int(npc_ids[1]), Value::Int(root_n)], &none).await.is_err(), "foreign same-root whole refused");
            assert!(call(&mut f.client, "Add", &params, &[Value::Int(npc_ids[1]), Value::Int(root_n)], &Value::dict([("qty", Value::Int(1))])).await.is_err(), "foreign same-root partial refused");
            assert!(call(&mut f.client, "Add", &wparams, &[Value::Int(far_root), Value::Int(other_sys)], &none).await.is_err(), "foreign-system source refused");
            // CorpDeliveries, internalModifier and a real fitting slot all refuse
            // on a non-ship destination; a module flag-0 batch refuses outright.
            for flag in [FLAG_CORP_DELIVERIES, FLAG_HIDDEN_MODIFIERS, FLAG_FITTING_FIRST] {
                assert!(call(&mut f.client, "Add", &wparams, &[Value::Int(npc_ids[0]), Value::Int(root_n)],
                    &Value::dict([("flag", Value::Int(i64::from(flag)))])).await.is_err(), "flag {flag} must refuse");
            }
            assert!(call(&mut f.client, "MultiAdd", &wparams, &[Value::list([Value::Int(npc_ids[0]), Value::Int(npc_ids[1])]), Value::Int(root_n)],
                &Value::dict([("flag", Value::Int(0))])).await.is_err(), "module flag-0 batch refuses");
            assert!(view(&f, root_n).await.unwrap().iter().any(|&(id, q, o, fl)| id == npc_ids[0] && q == 7 && o == npc_owner && fl == 0));
            // GUI shape: flag=None on a non-ship destination lands the hangar flag;
            // a partial move draws a new caller-owned id, the remainder keeps 0.
            let partial = call(&mut f.client, "Add", &wparams, &[Value::Int(npc_ids[1]), Value::Int(root_n)],
                &Value::dict([("qty", Value::Int(2)), ("flag", Value::None)])).await.unwrap().as_int().unwrap();
            assert_ne!(partial, npc_ids[1]);
            assert!(view(&f, root_own).await.unwrap().iter().any(|&(id, q, o, fl)| id == partial && q == 2 && o == owner && fl == FLAG_HANGAR));
            assert!(view(&f, root_n).await.unwrap().iter().any(|&(id, q, o, fl)| id == npc_ids[1] && q == 1 && o == npc_owner && fl == 0), "source remainder keeps owner and flag");
            // The matching remainder merges into that flag-4 stack: exact survivor.
            let whole = call(&mut f.client, "Add", &wparams, &[Value::Int(npc_ids[1]), Value::Int(root_n)], &none).await.unwrap().as_int().unwrap();
            assert_eq!(whole, partial);
            assert!(view(&f, root_own).await.unwrap().iter().any(|&(id, q, o, fl)| id == partial && q == 3 && o == owner && fl == FLAG_HANGAR), "merge survivor keeps caller owner");
            // MultiAdd flag=0 lands charges as cargo 5; a module later in the same
            // batch refuses after the charge already landed; it is not all-atomic.
            call(&mut f.client, "MultiAdd", &wparams, &[Value::list([Value::Int(npc_ids[2]), Value::Int(npc_ids[3])]), Value::Int(root_n)],
                &Value::dict([("flag", Value::Int(0))])).await.unwrap();
            assert!(view(&f, root_own).await.unwrap().iter().any(|&(_, q, o, fl)| q == 4 && o == owner && fl == FLAG_CARGO), "two charge stacks land as cargo");
            assert!(call(&mut f.client, "MultiAdd", &wparams, &[Value::list([Value::Int(npc_ids[4]), Value::Int(npc_ids[0])]), Value::Int(root_n)],
                &Value::dict([("flag", Value::Int(0))])).await.is_err(), "mixed batch refuses at the module");
            assert!(view(&f, root_own).await.unwrap().iter().any(|&(_, q, o, fl)| q == 6 && o == owner && fl == FLAG_CARGO), "the earlier charge still landed");
            assert!(view(&f, root_n).await.unwrap().iter().any(|&(id, q, o, fl)| id == npc_ids[0] && q == 7 && o == npc_owner && fl == 0), "the refused module moved nothing");
            // A whole foreign stack with flag 5 keeps its original id, switches to
            // caller owner and flag 5.
            let whole_id = call(&mut f.client, "Add", &wparams, &[Value::Int(npc_ids[0]), Value::Int(root_n)],
                &Value::dict([("flag", Value::Int(i64::from(FLAG_CARGO)))])).await.unwrap().as_int().unwrap();
            assert_eq!(whole_id, npc_ids[0]);
            assert!(view(&f, root_own).await.unwrap().iter().any(|&(id, q, o, fl)| id == npc_ids[0] && q == 7 && o == owner && fl == FLAG_CARGO));
            // The watching pilot saw every real payload shape: broker split emits
            // the old source row plus a new slice whose only change is
            // IX_LOCATION_ID 0; an Add partial reports old root location and old
            // source flag; the whole move reports both old owner and old flag.
            let changes: Vec<(i64, i64, Vec<(Value, Value)>)> = std::iter::from_fn(|| notices.try_recv().ok())
                .filter(|(name, _)| name == "OnItemChange")
                .filter_map(|(_, args)| Some((
                    field(&args[0], "itemID")?.as_int()?,
                    field(&args[0], "locationID")?.as_int()?,
                    match args.get(1) { Some(Value::Dict(pairs)) => pairs.clone(), _ => Vec::new() })))
                .collect();
            let dict_of = |id: i64, location: i64| changes.iter().find(|&&(i, loc, _)| i == id && loc == location).map(|(_, _, d)| d.clone());
            assert_eq!(dict_of(created, root_own), Some(vec![(Value::Int(IX_LOCATION_ID), Value::Int(0))]),
                "broker split slice carries only location 0");
            assert!(dict_of(own_ids[0], root_own).is_some(), "the split also reports the source row");
            let partial_change = dict_of(partial, root_own).expect("partial destination payload");
            assert!(partial_change.iter().any(|(k, v)| *k == Value::Int(IX_LOCATION_ID) && *v == Value::Int(root_n))
                && partial_change.iter().any(|(k, v)| *k == Value::Int(IX_FLAG) && *v == Value::Int(0)),
                "Add partial reports old root and old flag");
            let whole_source = dict_of(npc_ids[0], infinity_core::consts::LOCATION_JUNKYARD).expect("whole source removal");
            assert!(whole_source.iter().any(|(k, v)| *k == Value::Int(IX_LOCATION_ID) && *v == Value::Int(root_n)),
                "whole source removal carries the old root");
            let whole_change = dict_of(npc_ids[0], root_own).expect("whole destination payload");
            assert!(whole_change.iter().any(|(k, v)| *k == Value::Int(IX_OWNER_ID) && *v == Value::Int(npc_owner))
                && whole_change.iter().any(|(k, v)| *k == Value::Int(IX_FLAG) && *v == Value::Int(0)),
                "destination payload carries the real owner and flag change");
            // Own same-root: flag=None really updates 0→4 in place; only an equal
            // flag is a true no-op; a partial with flag 5 splits a flag-5 slice.
            let w2 = bound_params(&mut f.client, "GetInventoryFromId", &[Value::Int(root_own2)]).await;
            let same = call(&mut f.client, "Add", &w2, &[Value::Int(own2_ids[0]), Value::Int(root_own2)], &none).await.unwrap().as_int().unwrap();
            assert_eq!(same, own2_ids[0]);
            assert_eq!(view(&f, root_own2).await.unwrap(), vec![(own2_ids[0], 2, owner, FLAG_HANGAR)]);
            let noop = call(&mut f.client, "Add", &w2, &[Value::Int(own2_ids[0]), Value::Int(root_own2)],
                &Value::dict([("flag", Value::Int(i64::from(FLAG_HANGAR)))])).await.unwrap().as_int().unwrap();
            assert_eq!(noop, own2_ids[0]);
            assert_eq!(view(&f, root_own2).await.unwrap(), vec![(own2_ids[0], 2, owner, FLAG_HANGAR)], "an equal flag is the only true no-op");
            let slice = call(&mut f.client, "Add", &w2, &[Value::Int(own2_ids[0]), Value::Int(root_own2)],
                &Value::dict([("qty", Value::Int(1)), ("flag", Value::Int(i64::from(FLAG_CARGO)))])).await.unwrap().as_int().unwrap();
            assert_ne!(slice, own2_ids[0]);
            let own2 = view(&f, root_own2).await.unwrap();
            assert!(own2.iter().any(|&(id, q, o, fl)| id == slice && q == 1 && o == owner && fl == FLAG_CARGO)
                && own2.iter().any(|&(id, q, o, fl)| id == own2_ids[0] && q == 1 && o == owner && fl == FLAG_HANGAR));
            let same_root_slice = call(&mut f.client, "Add", &wparams, &[Value::Int(own_ids[1]), Value::Int(root_own)],
                &Value::dict([("qty", Value::Int(1)), ("flag", Value::None)])).await.unwrap().as_int().unwrap();
            assert_ne!(same_root_slice, own_ids[1]);
            let own = view(&f, root_own).await.unwrap();
            assert!(own.iter().any(|&(id, q, o, fl)| id == same_root_slice && q == 1 && o == owner && fl == FLAG_HANGAR)
                && own.iter().any(|&(id, q, o, fl)| id == own_ids[1] && q == 3 && o == owner && fl == 0));
            assert_eq!(f.db.query_one("SELECT count(*) AS c FROM game.items WHERE item_id=ANY($1)",
                &[&vec![root_n, root_own, root_own2, partial, whole, slice, same_root_slice, created, npc_ids[0], npc_ids[1]]]).await.unwrap().get::<_, i64>("c"), 0);
            // A far pilot is denied; the owner's own root trashes; a foreign pilot
            // cannot trash what it does not own; a busy source refuses reads.
            let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
            let (_listener, _fp, mut foreign) = crate::services::tests::connected_client(f.db.clone(), f.sde.clone(), f.world.clone(), std::sync::Arc::new(crate::config::Config::default()), login).await;
            foreign.session.apply(vec![("charid", Value::Int(other.character)), ("shipid", Value::Int(other_ship)), ("solarsystemid", Value::Int(f.system)), ("solarsystemid2", Value::Int(f.system))]);
            assert!(crate::services::inventory::trashing::trash_items(&mut foreign, &[root_own2], f.system).await.is_err());
            crate::services::inventory::trashing::trash_items(&mut f.client, &[root_own2], f.system).await.unwrap();
            assert!(view(&f, root_own2).await.is_none());
            f.world.add_ship(f.system, ball(other_ship, f.ship_type, other.character, Some(other.character), [1e9, 0.0, 0.0])).await.unwrap();
            assert!(dispatch(&mut foreign, &Call { service: "invbroker", method: "GetInventoryFromId",
                args: &[Value::Int(root_n)], kwargs: &kwargs, bound: false, bind_params: None }).await.is_err(), "a far pilot is denied");
            // A real held guard keeps the claim Claimed: the read stays busy
            // until the same guard finishes, never through a leaked Arc.
            let held = f.world.with(f.system, move |s| {
                let access = NpcWreckAccess::GmCleanup { character: 1 };
                let NpcWreckLookup::Ready(view) = s.npc_wreck_recovery_view(&access, root_n) else { panic!("view") };
                match s.claim_npc_wreck(&view.identity, access, NpcWreckOperation::Delete { items: vec![root_n], persistent_roots: vec![], persistent: vec![] }) {
                    NpcWreckLookup::Ready(guard) => guard, _ => panic!("claim") }
            }).await.unwrap();
            assert!(call(&mut f.client, "List", &params, &[], &none).await.is_err(), "busy source refuses");
            held.finish(NpcWreckDisposition::RolledBack).await;
            f.world.remove_ball(f.system, other_ship).await;
            fixture::cleanup(&f.db, other).await.unwrap();
        }).await;
        for root in [root_n, root_own, root_own2] { f.world.remove_ball(f.system, root).await; }
        f.world.remove_ball(other_sys, far_root).await;
        db_unguard(&f.db, &tag).await;
        fixture::cleanup(&f.db, f.fixture).await.unwrap();
        scenario.unwrap();
    }

    mod pickup;

    #[tokio::test]
    #[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
    async fn commit_in_flight_cancel_proves_once_through_real_fence() {
        let mut f = space().await;
        let owner = f.fixture.character;
        let (root, loot_ids) = spawn(&f, owner, [50.0, 0.0, 0.0], vec![(f.loot_type, 5), (f.loot_type, 2)]).await;
        let (rollback_root, rollback_ids) = spawn(&f, owner, [60.0, 0.0, 0.0], vec![(f.loot_type, 4)]).await;
        let control = control_client().await;
        let gate = 0x5abc0001_i32 ^ (owner as i32);
        let tag = format!("npc_gate_{owner}");
        let business_name = format!("{}_b", f.pool_name);
        let commit_db = named_db(&business_name).await;
        commit_db.query_one("SELECT 1", &[]).await.unwrap();
        control.observe_pools(&[f.pool_name.clone(), business_name.clone()]).await;
        let scenario = crate::tests::catch_scenario(async {
            // Deferred commit gate: fires only inside the real take's COMMIT
            // and only for this fixture's canonical take-receipt sources.
            control.gate_fn(&tag, gate).await;
            control.client.execute(&format!("CREATE CONSTRAINT TRIGGER {tag} AFTER INSERT ON game.progress_events DEFERRABLE INITIALLY DEFERRED
                FOR EACH ROW WHEN ((NEW.source_key LIKE 'NpcLootTaken:{root}/%') OR (NEW.source_key LIKE 'NpcLootTaken:{rollback_root}/%'))
                EXECUTE FUNCTION game.{tag}()"), &[]).await.unwrap();
            control.hold(gate).await;
            // The business call runs on isolated pool B; the proof stays on P.
            let proof_db = std::mem::replace(&mut f.client.db, commit_db.clone());
            let none = Value::Dict(vec![]);
            let params = bound_params(&mut f.client, "GetInventoryFromId", &[Value::Int(f.ship)]).await;
            let args = [Value::Int(loot_ids[0]), Value::Int(root)];
            let mut add: Pin<Box<dyn Future<Output = Result<Value, CallError>> + '_>> =
                Box::pin(call(&mut f.client, "Add", &params, &args, &none));
            let (ready, business_pid) = drive_gated(&f.db, &control, &mut add, "%commit%").await;
            assert!(ready.is_none(), "business request completed before COMMIT gate: {ready:?}");
            // Cancel the owned future while the COMMIT is still gated: the
            // already-sent commit is not implicitly rolled back; the guard
            // seals Unknown through the mailbox.
            drop(add);
            let claim = busy_claim(&f.world, f.system, root).await.expect("the take must have claimed");
            let taken_id = claim.taken().expect("take claim carries its slice").item_id;
            let key = claim.receipt().expect("take claim carries a receipt").source_key().as_str().to_string();
            wait_claim(&f, &claim, Some(NpcWreckClaimState::Unknown)).await;
            // A fresh public Add on pool P only helps the proof: its reconcile
            // waits on the original backend's real operation fence.
            f.client.db = proof_db;
            let mut help: Pin<Box<dyn Future<Output = Result<Value, CallError>> + '_>> =
                Box::pin(call(&mut f.client, "Add", &params, &args, &none));
            let first = finish_proof(&f.db, &control, business_pid, &mut help).await;
            assert!(first.is_err(), "the helper finishes the old delta, never its own");
            drop(help);
            let row = f.db.query_one("SELECT location_id, flag_id, quantity FROM game.items WHERE item_id=$1", &[&taken_id]).await.unwrap();
            assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("quantity")), (f.ship, FLAG_CARGO, 5));
            assert_eq!(f.db.query_one("SELECT count(*) AS c FROM game.progress_events WHERE source_key=$1", &[&key]).await.unwrap().get::<_, i64>("c"), 1);
            assert_eq!(view(&f, root).await.unwrap(), vec![(loot_ids[1], 2, owner, 0)], "the applied delta finished once");
            // Definite rollback: an AFTER INSERT gate stops the write inside
            // the transaction; dropping the unfinished future rolls it back
            // and the source is restored whole.
            control.client.execute(&format!("CREATE TRIGGER {tag}_insert AFTER INSERT ON game.items FOR EACH ROW
                WHEN (NEW.location_id={rollback_root}) EXECUTE FUNCTION game.{tag}()"), &[]).await.unwrap();
            control.hold(gate).await;
            let proof_db = std::mem::replace(&mut f.client.db, commit_db.clone());
            let partial = Value::dict([("qty", Value::Int(2))]);
            let args = [Value::Int(rollback_ids[0]), Value::Int(rollback_root)];
            let mut add: Pin<Box<dyn Future<Output = Result<Value, CallError>> + '_>> =
                Box::pin(call(&mut f.client, "Add", &params, &args, &partial));
            let (ready, business_pid) = drive_gated(&f.db, &control, &mut add, "%insert%").await;
            assert!(ready.is_none(), "rollback request completed before INSERT gate: {ready:?}");
            drop(add);
            let claim = busy_claim(&f.world, f.system, rollback_root).await.expect("rollback claim");
            let aborted_slice = claim.taken().expect("take claim carries its slice").item_id;
            let rollback_key = claim.receipt().expect("take claim carries a receipt").source_key().as_str().to_string();
            wait_claim(&f, &claim, Some(NpcWreckClaimState::Unknown)).await;
            f.client.db = proof_db;
            let mut help: Pin<Box<dyn Future<Output = Result<Value, CallError>> + '_>> =
                Box::pin(call(&mut f.client, "Add", &params, &args, &partial));
            let first = finish_proof(&f.db, &control, business_pid, &mut help).await;
            assert!(first.is_err(), "the public helper proves rollback but does not start a new take");
            drop(help);
            assert!(matches!(f.world.with(f.system, { let claim = claim.clone(); move |s| s.npc_wreck_claim_status(&claim) }).await, Some(None)));
            assert!(f.db.query_opt("SELECT 1 FROM game.progress_events WHERE source_key=$1", &[&rollback_key]).await.unwrap().is_none());
            assert!(f.db.query_opt("SELECT 1 FROM game.items WHERE item_id=$1", &[&aborted_slice]).await.unwrap().is_none());
            assert_eq!(view(&f, rollback_root).await.unwrap(), vec![(rollback_ids[0], 4, owner, 0)]);
            control.client.execute(&format!("DROP TRIGGER IF EXISTS {tag}_insert ON game.items"), &[]).await.unwrap();
            let new_slice = call(&mut f.client, "Add", &params, &[Value::Int(rollback_ids[0]), Value::Int(rollback_root)], &partial).await.unwrap().as_int().unwrap();
            assert_ne!(new_slice, aborted_slice, "a failed partial take burns its reserved slice id");
            assert_ne!(new_slice, rollback_ids[0]);
            assert_eq!(view(&f, rollback_root).await.unwrap(), vec![(rollback_ids[0], 2, owner, 0)]);
            let row = f.db.query_one("SELECT location_id, quantity FROM game.items WHERE item_id=$1", &[&new_slice]).await.unwrap();
            assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i64>("quantity")), (f.ship, 2));
            assert_eq!(f.db.query_one("SELECT count(*) AS c FROM game.progress_events WHERE source_key LIKE $1", &[&format!("NpcLootTaken:{rollback_root}/%")]).await.unwrap().get::<_, i64>("c"), 1);
        }).await;
        let mut cleanup = Vec::new();
        let release = crate::tests::catch_scenario(async {
            control.client.execute("COMMIT", &[]).await.expect("release test gate during cleanup");
        }).await;
        let mut safe = release.is_ok();
        cleanup.push(release);
        if safe {
            let pools = crate::tests::catch_scenario(async { control.wait_pools(&[f.pool_name.clone(), business_name]).await; }).await;
            let backends = crate::tests::catch_scenario(async { control.wait_backends().await; }).await;
            safe = pools.is_ok() && backends.is_ok();
            cleanup.extend([pools, backends]);
        }
        if safe {
            cleanup.push(crate::tests::catch_scenario(async {
                control.client.execute(&format!("DROP TRIGGER IF EXISTS {tag}_insert ON game.items"), &[]).await.unwrap();
            }).await);
            cleanup.push(crate::tests::catch_scenario(async {
                control.gate_uninstall(&tag, &["game.progress_events", "game.items"]).await;
            }).await);
            cleanup.push(crate::tests::catch_scenario(async {
                for key in [format!("NpcLootTaken:{root}/%"), format!("NpcLootTaken:{rollback_root}/%")] {
                    f.db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE source_key LIKE $1)", &[&key]).await.unwrap();
                    f.db.execute("DELETE FROM game.progress_events WHERE source_key LIKE $1", &[&key]).await.unwrap();
                }
            }).await);
        }
        drop(commit_db);
        let mut driver = control.into_driver();
        cleanup.push(crate::tests::catch_scenario(async { wait_driver(&mut driver).await; }).await);
        if safe {
            cleanup.push(crate::tests::catch_scenario(async { fixture::cleanup(&f.db, f.fixture).await.unwrap(); }).await);
        }
        report_cleanup_failures(&cleanup);
        if let Err(panic) = scenario {
            std::panic::resume_unwind(panic);
        }
        for outcome in cleanup { if let Err(panic) = outcome { std::panic::resume_unwind(panic); } }
    }

    #[tokio::test]
    #[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
    async fn mixed_delete_commit_cancel_proves_preimage_closure() {
        let mut f = space().await;
        f.client.login.role = infinity_core::consts::GM_ROLES;
        let owner = f.fixture.character;
        let (root, _) = spawn(&f, owner, [50.0, 0.0, 0.0], vec![(f.loot_type, 3)]).await;
        let container_type = f.sde.types.iter().find(|t| crate::services::inventory::hold_storage::is_ordinary_container(&f.sde, t.type_id)
            && !f.sde.typelists.is_untrashable(t.type_id, t.group_id.unwrap_or_default(), t.category_id.unwrap_or_default())).unwrap().type_id;
        let persistent = crate::dao::items::creation::positioned(&f.db, container_type, owner, f.system, [60.0, 0.0, 0.0]).await.unwrap();
        let child = crate::dao::items::transfers::insert(&f.db, f.loot_type, owner, persistent, 0, 1).await.unwrap();
        let radius = f.sde.types.get(container_type).and_then(|t| t.radius).unwrap_or(100.0) as f32;
        f.world.add_ship(f.system, crate::services::combat::groups_health::wreck::debris_ball(
            persistent, container_type, owner, 1, [60.0, 0.0, 0.0], radius, None, false, crate::space::now_ms())).await.unwrap();
        let control = control_client().await;
        let gate = 0x5abc0007_i32 ^ (owner as i32);
        let tag = format!("npc_del_gate_{owner}");
        let business_name = format!("{}_b", f.pool_name);
        let commit_db = named_db(&business_name).await;
        commit_db.query_one("SELECT 1", &[]).await.unwrap();
        control.observe_pools(&[f.pool_name.clone(), business_name.clone()]).await;
        async fn rpc(c: &mut Client) -> crate::services::CallResult {
            crate::services::slash::tests::support::rpc(c, "/testclear").await
        }
        let scenario = crate::tests::catch_scenario(async {
            // The delete-preimage gate fires inside the real COMMIT only for
            // this fixture's persistent root.
            control.gate_fn(&tag, gate).await;
            control.client.execute(&format!("CREATE CONSTRAINT TRIGGER {tag} AFTER DELETE ON game.items DEFERRABLE INITIALLY DEFERRED
                FOR EACH ROW WHEN (OLD.item_id={persistent}) EXECUTE FUNCTION game.{tag}()"), &[]).await.unwrap();
            control.hold(gate).await;
            let proof_db = std::mem::replace(&mut f.client.db, commit_db.clone());
            let mut slash: Pin<Box<dyn Future<Output = crate::services::CallResult> + '_>> = Box::pin(rpc(&mut f.client));
            let (ready, business_pid) = drive_gated(&f.db, &control, &mut slash, "%commit%").await;
            assert!(ready.is_none(), "cleanup completed before COMMIT gate: {ready:?}");
            // The transient claim froze its persistent preimage before cancel:
            // a concurrent cleanup can only be busy, never a second run.
            let claim = busy_claim(&f.world, f.system, root).await.expect("the batch claimed the transient root");
            drop(slash);
            wait_claim(&f, &claim, Some(NpcWreckClaimState::Unknown)).await;
            f.client.db = proof_db;
            // The second public /testclear waits on the original backend's real
            // NpcWreckDelete operation fence, then the preimage proof applies.
            let mut help: Pin<Box<dyn Future<Output = crate::services::CallResult> + '_>> = Box::pin(rpc(&mut f.client));
            finish_proof(&f.db, &control, business_pid, &mut help).await.expect("helper cleanup completed");
            drop(help);
            // The frozen preimage proved: persistent root and child are gone,
            // the transient delta applied once, no receipt was required.
            assert!(f.world.ball(f.system, root).await.is_none() && f.world.ball(f.system, persistent).await.is_none());
            assert!(f.db.query_opt("SELECT 1 FROM game.items WHERE item_id = ANY($1)", &[&vec![persistent, child]]).await.unwrap().is_none());
            // Delete-specific variants on the same setup: an after-delete gate
            // inside the transaction rolls back cleanly, and a recreated
            // preimage row keeps the proof Unknown until the closure holds.
            let persistent2 = crate::dao::items::creation::positioned(&f.db, container_type, owner, f.system, [80.0, 0.0, 0.0]).await.unwrap();
            let child2 = crate::dao::items::transfers::insert(&f.db, f.loot_type, owner, persistent2, 0, 1).await.unwrap();
            let (root_b, _) = spawn(&f, owner, [90.0, 0.0, 0.0], vec![]).await;
            f.world.add_ship(f.system, crate::services::combat::groups_health::wreck::debris_ball(
                persistent2, container_type, owner, 1, [80.0, 0.0, 0.0], radius, None, false, crate::space::now_ms())).await.unwrap();
            control.client.execute(&format!("CREATE TRIGGER {tag}_del AFTER DELETE ON game.items FOR EACH ROW
                WHEN (OLD.item_id={persistent2}) EXECUTE FUNCTION game.{tag}()"), &[]).await.unwrap();
            control.hold(gate).await;
            let proof_db = std::mem::replace(&mut f.client.db, commit_db.clone());
            let mut slash: Pin<Box<dyn Future<Output = crate::services::CallResult> + '_>> = Box::pin(rpc(&mut f.client));
            let (ready, _) = drive_gated(&f.db, &control, &mut slash, "%delete%").await;
            assert!(ready.is_none(), "cleanup completed before DELETE gate: {ready:?}");
            drop(slash);
            let claim = busy_claim(&f.world, f.system, root_b).await.expect("the batch claimed the transient root");
            wait_claim(&f, &claim, Some(NpcWreckClaimState::Unknown)).await;
            f.client.db = proof_db;
            control.release().await;
            rpc(&mut f.client).await.expect("after rollback the helper cleanup commits the whole batch");
            assert!(f.db.query_opt("SELECT 1 FROM game.items WHERE item_id = ANY($1)", &[&vec![persistent2, child2]]).await.unwrap().is_none()
                && f.world.ball(f.system, root_b).await.is_none(), "the whole batch eventually lands");
            // A recreated preimage row is a live contradiction: the proof
            // stays sealed until the frozen closure is empty again.
            let persistent3 = crate::dao::items::creation::positioned(&f.db, container_type, owner, f.system, [100.0, 0.0, 0.0]).await.unwrap();
            let child3 = crate::dao::items::transfers::insert(&f.db, f.loot_type, owner, persistent3, 0, 1).await.unwrap();
            let child3_row = f.db.query_one("SELECT type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items WHERE item_id=$1", &[&child3]).await.unwrap();
            let (root_c, _) = spawn(&f, owner, [110.0, 0.0, 0.0], vec![]).await;
            f.world.add_ship(f.system, crate::services::combat::groups_health::wreck::debris_ball(
                persistent3, container_type, owner, 1, [100.0, 0.0, 0.0], radius, None, false, crate::space::now_ms())).await.unwrap();
            control.client.execute(&format!("CREATE CONSTRAINT TRIGGER {tag}_c AFTER DELETE ON game.items DEFERRABLE INITIALLY DEFERRED
                FOR EACH ROW WHEN (OLD.item_id={persistent3}) EXECUTE FUNCTION game.{tag}()"), &[]).await.unwrap();
            control.hold(gate).await;
            let proof_db = std::mem::replace(&mut f.client.db, commit_db.clone());
            let mut slash: Pin<Box<dyn Future<Output = crate::services::CallResult> + '_>> = Box::pin(rpc(&mut f.client));
            let (ready, _) = drive_gated(&f.db, &control, &mut slash, "%commit%").await;
            assert!(ready.is_none(), "cleanup completed before COMMIT gate: {ready:?}");
            drop(slash);
            let claim = busy_claim(&f.world, f.system, root_c).await.expect("the batch claimed the transient root");
            wait_claim(&f, &claim, Some(NpcWreckClaimState::Unknown)).await;
            f.client.db = proof_db;
            control.release().await;
            // Wait for the committed delete, then recreate one preimage row.
            tokio::time::timeout(WAIT, async {
                while f.db.query_opt("SELECT 1 FROM game.items WHERE item_id=$1", &[&persistent3]).await.unwrap().is_some() {
                    tokio::task::yield_now().await;
                }
            }).await.expect("committed delete never became visible");
            f.db.execute("INSERT INTO game.items (item_id, type_id, owner_id, location_id, flag_id, quantity, singleton)
                VALUES ($1,$2,$3,$4,$5,$6,$7)", &[&child3, &child3_row.get::<_, i32>("type_id"), &child3_row.get::<_, i64>("owner_id"),
                &child3_row.get::<_, i64>("location_id"), &child3_row.get::<_, i32>("flag_id"), &child3_row.get::<_, i64>("quantity"),
                &child3_row.get::<_, bool>("singleton")]).await.unwrap();
            let mut contradict: Pin<Box<dyn Future<Output = crate::services::CallResult> + '_>> = Box::pin(rpc(&mut f.client));
            let stale = tokio::time::timeout(WAIT, contradict.as_mut()).await.expect("contradictory reconcile never returned");
            assert!(stale.is_err(), "a live preimage contradiction refuses, never double-applies");
            drop(contradict);
            assert!(matches!(f.world.with(f.system, { let claim = claim.clone(); move |s| s.npc_wreck_claim_status(&claim) }).await,
                Some(Some(NpcWreckClaimState::Unknown))), "a recreated preimage row keeps the seal");
            f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&child3]).await.unwrap();
            rpc(&mut f.client).await.expect("once the closure is empty again the proof commits");
            assert!(f.world.ball(f.system, root_c).await.is_none());
        }).await;
        let mut cleanup = Vec::new();
        let release = crate::tests::catch_scenario(async {
            control.client.execute("COMMIT", &[]).await.expect("release test gate during cleanup");
        }).await;
        let mut safe = release.is_ok();
        cleanup.push(release);
        if safe {
            let pools = crate::tests::catch_scenario(async { control.wait_pools(&[f.pool_name.clone(), business_name]).await; }).await;
            let backends = crate::tests::catch_scenario(async { control.wait_backends().await; }).await;
            safe = pools.is_ok() && backends.is_ok();
            cleanup.extend([pools, backends]);
        }
        if safe {
            cleanup.push(crate::tests::catch_scenario(async {
                control.client.execute(&format!("DROP TRIGGER IF EXISTS {tag}_del ON game.items"), &[]).await.unwrap();
            }).await);
            cleanup.push(crate::tests::catch_scenario(async {
                control.client.execute(&format!("DROP TRIGGER IF EXISTS {tag}_c ON game.items"), &[]).await.unwrap();
            }).await);
            cleanup.push(crate::tests::catch_scenario(async {
                control.gate_uninstall(&tag, &["game.items"]).await;
            }).await);
            cleanup.push(crate::tests::catch_scenario(async {
                f.db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![persistent, child]]).await.unwrap();
            }).await);
        }
        drop(commit_db);
        let mut driver = control.into_driver();
        cleanup.push(crate::tests::catch_scenario(async { wait_driver(&mut driver).await; }).await);
        if safe {
            cleanup.push(crate::tests::catch_scenario(async { fixture::cleanup(&f.db, f.fixture).await.unwrap(); }).await);
        }
        report_cleanup_failures(&cleanup);
        if let Err(panic) = scenario {
            std::panic::resume_unwind(panic);
        }
        for outcome in cleanup { if let Err(panic) = outcome { std::panic::resume_unwind(panic); } }
    }

    #[tokio::test]
    #[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and INFINITY_TEST_ENABLE_IDLE_STOP=1 (own process)"]
    async fn unload_drops_transient_and_keeps_taken_rows() {
        crate::tests::init_config();
        assert!(crate::config::game().gm.performance_testing, "requires INFINITY_TEST_ENABLE_IDLE_STOP=1");
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = crate::tests::shared_sde(&db).await;
        let system = sde.map.systems().find(|s| sde.map.is_public_system(s.solar_system_id)).unwrap().solar_system_id;
        let mut f = space_at(system).await;
        let owner = f.fixture.character;
        let (root, loot_ids) = spawn(&f, owner, [50.0, 0.0, 0.0], vec![(f.loot_type, 5)]).await;
        let (root2, _) = spawn(&f, owner, [80.0, 0.0, 0.0], vec![(f.loot_type, 1)]).await;
        let scenario = crate::tests::catch_scenario(async {
            // A real partial take first: the durable row must survive the unload.
            let params = bound_params(&mut f.client, "GetInventoryFromId", &[Value::Int(f.ship)]).await;
            let moved = call(&mut f.client, "Add", &params, &[Value::Int(loot_ids[0]), Value::Int(root)], &Value::dict([("qty", Value::Int(2))])).await.unwrap().as_int().unwrap();
            assert!(f.db.query_opt("SELECT 1 FROM game.items WHERE item_id=$1", &[&moved]).await.unwrap().is_some());
            // Drain the pool with legal empty spawns (one reserved id each)
            // inside a single actor turn, then park a real salvage of the
            // still-looted root: no replacement id leaves it WaitingIds.
            let parked = f.world.with(f.system, move |s| {
                while let Some(id) = s.npc_wreck_spawn(f.wreck_type, owner, 1, [0.0; 3], 1.0, None, vec![]) {
                    s.remove_ball(id);
                }
                let access = NpcWreckAccess::Recovery {
                    character: owner, ship: f.ship, source: f.ship,
                    source_token: s.capture_ball(f.ship).expect("pilot ball"), range: 1_000_000.0,
                    attempt: RecoveryAttempt::Module { master: 1, stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) } };
                let NpcWreckLookup::Ready(view) = s.npc_wreck_recovery_view(&access, root) else { panic!("view") };
                let event = GameEvent::WreckSalvaged {
                    char_id: owner, wreck_id: root, wreck_type: f.wreck_type, occurred_at: 0,
                    context: FactContext { corporation_id: None, alliance_id: None, faction_id: None,
                        ship_id: Some(f.ship), ship_type_id: Some(f.ship_type), system_id: Some(f.system),
                        station_id: None, origin: FactOrigin::Natural } };
                match s.claim_npc_wreck(&view.identity, access, NpcWreckOperation::Salvage {
                    character: owner, destination: f.ship, yields: vec![(f.loot_type, 2)], event }) {
                    NpcWreckLookup::Ready(guard) if !guard.claim().prepared() => { let claim = guard.claim().clone(); guard.park(); claim },
                    _ => panic!("salvage claim must park") }
            }).await.unwrap();
            // Only with the pilot ball gone does the check reach npc_wreck_claims.
            f.world.remove_ball(f.system, f.ship).await;
            let reason = f.world.with(f.system, move |s| { s.tick(s.now_ms() + 2_000); s.unload_blocked() }).await.unwrap();
            assert_eq!(reason, Some("npc_wreck_claims"), "WaitingIds blocks");
            // Removing the root ball keeps the parked claim blocking via tombstone.
            f.world.remove_ball(f.system, root).await;
            let reason = f.world.with(f.system, move |s| { s.tick(s.now_ms() + 2_000); s.unload_blocked() }).await.unwrap();
            assert_eq!(reason, Some("npc_wreck_claims"), "tombstoned claim blocks");
            let freed = f.world.with(f.system, move |s| s.cancel_waiting_npc_wreck(&parked)).await.unwrap();
            assert!(freed);
            let reason = f.world.with(f.system, move |s| { s.tick(s.now_ms() + 2_000); s.unload_blocked() }).await.unwrap();
            assert_ne!(reason, Some("npc_wreck_claims"), "the previous tombstone no longer blocks");
            // Claimed and Unknown states block just the same on a second root;
            // the real guard stays held until the explicit finish.
            let mut held = f.world.with(f.system, move |s| {
                let access = NpcWreckAccess::GmCleanup { character: 1 };
                let NpcWreckLookup::Ready(view) = s.npc_wreck_recovery_view(&access, root2) else { panic!("view") };
                match s.claim_npc_wreck(&view.identity, access, NpcWreckOperation::Delete { items: vec![root2], persistent_roots: vec![], persistent: vec![] }) {
                    NpcWreckLookup::Ready(guard) => guard, _ => panic!("claim") }
            }).await.unwrap();
            let reason = f.world.with(f.system, move |s| { s.tick(s.now_ms() + 2_000); s.unload_blocked() }).await.unwrap();
            assert_eq!(reason, Some("npc_wreck_claims"), "Claimed blocks");
            let c = held.claim().clone();
            held.may_execute();
            drop(held); // an executing guard's real Drop posts Unknown through the mailbox
            wait_claim(&f, &c, Some(NpcWreckClaimState::Unknown)).await;
            let reason = f.world.with(f.system, move |s| { s.tick(s.now_ms() + 2_000); s.unload_blocked() }).await.unwrap();
            assert_eq!(reason, Some("npc_wreck_claims"), "Unknown blocks");
            f.world.with(f.system, move |s| { let c = c.clone(); s.finish_npc_wreck(&c, NpcWreckDisposition::RolledBack); }).await;
            // The drained pool armed one real refill. One successful empty
            // spawn proves that block arrived; remove it in the same turn.
            tokio::time::timeout(WAIT, async {
                loop {
                    let spawned = f.world.with(f.system, move |s| {
                        let id = s.npc_wreck_spawn(f.wreck_type, owner, 1, [0.0; 3], 1.0, None, vec![]);
                        if let Some(id) = id { s.remove_ball(id); }
                        id.is_some()
                    }).await.unwrap();
                    if spawned { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("wreck id refill never delivered one id");
            f.world.with(f.system, move |s| { s.tick(s.now_ms() + 2_000); }).await;
            let pre_blocker = f.world.with(f.system, |s| s.unload_blocked()).await.unwrap();
            let report = f.world.unload_idle_systems().await.unwrap();
            assert_eq!(report.unloaded, 1, "free transient root must not block unload: {report:?}, pre-unload blocker {pre_blocker:?}");
            // Real cold reload: transient state is gone; the taken row persists.
            assert!(f.world.with(f.system, |_| ()).await.is_none(), "the old actor must be gone");
            f.world.system(f.system).await.unwrap();
            assert_eq!(f.world.with(f.system, move |s| s.ball(root).is_some()).await, Some(false));
            assert_eq!(view(&f, root).await, None);
            let row = f.db.query_one("SELECT location_id, quantity FROM game.items WHERE item_id=$1", &[&moved]).await.unwrap();
            assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i64>("quantity")), (f.ship, 2));
        }).await;
        let mut cleanup = Vec::new();
        cleanup.push(crate::tests::catch_scenario(async { f.world.remove_ball(f.system, f.ship).await; }).await);
        cleanup.push(crate::tests::catch_scenario(async { fixture::cleanup(&f.db, f.fixture).await.unwrap(); }).await);
        drop(db);
        report_cleanup_failures(&cleanup);
        if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
        for outcome in cleanup { if let Err(panic) = outcome { std::panic::resume_unwind(panic); } }
    }
}
