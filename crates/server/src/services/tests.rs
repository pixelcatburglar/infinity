use super::*;
use std::sync::Arc;

pub(crate) mod fixture;
mod account;
mod brawl;
mod item_descriptor;
mod keeper;
mod node_graph;
mod operations;
mod world_tasks;
mod ship_adaption;
mod travel;
mod survey_pvp;
mod platform_login;
mod platform_tools;
mod support;
mod season;
mod paid_platform;
mod tourney;

pub(super) fn database_message(error: &anyhow::Error) -> Option<&str> {
    match error.downcast_ref::<crate::db::Error>()? {
        crate::db::Error::Postgres(error) => error.as_db_error().map(|error| error.message()),
        _ => None,
    }
}

pub(crate) async fn client(db: crate::db::Db, character: i64) -> (Client, tokio::net::TcpStream) {
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let conn = crate::conn::Conn::new(stream, address);
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut client = Client::new(conn, cfg, db, sde, chat, world, login);
    client.session.apply(vec![("charid", Value::Int(character))]);
    (client, peer)
}

#[test]
fn unit_from_bits_covers_unit_interval() {
    assert_eq!(unit_from_bits(0), 0.0);
    let max = unit_from_bits(u64::MAX);
    assert!(max < 1.0 && max > 0.999);
    assert_eq!(unit_from_bits(1u64 << 63), 0.5);
    assert_eq!(unit_from_bits(0x4000_0000_0000_0000), 0.25);
}

#[test]
fn surface_offset_includes_radius() {
    let center = [1.0e9, -2.0e9, 3.0e9];
    for radius in [0.0, 6_000_000.0, 1.0e12] {
        let clearance = 1_000_000.0;
        let actual = crate::space::dist(center, off_surface(center, radius, clearance));
        let expected = radius + clearance;
        assert!((actual - expected).abs() < expected * 1e-12, "{actual} != {expected}");
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn login_campaign_fixed_dispatch_roundtrips_none() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    for method in ["get_client_campaign_state", "claim_reward"] {
        let call = Call { service: "loginCampaignManager", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::None);
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::None);
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn rw_nearest_stations_dispatch_roundtrips_without_session_changes() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    let before: Vec<_> = client.session.attributes().map(|(key, value)| (key.to_owned(), value.clone())).collect();
    let sid = client.session.sid;
    for system in [Value::Int(1), Value::Int(2), Value::None] {
        let args = [system];
        let call = Call { service: "RWManager", method: "get_closest_rw_stations", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::Dict(vec![]));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::Dict(vec![]));
        assert_eq!(client.session.sid, sid);
        assert_eq!(client.session.attributes().map(|(key, value)| (key.to_owned(), value.clone())).collect::<Vec<_>>(), before);
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn structure_empty_reads_dispatch_without_session_changes() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    let before: Vec<_> = client.session.attributes().map(|(key, value)| (key.to_owned(), value.clone())).collect();
    let sid = client.session.sid;
    assert!(client.session.int("structureid").is_none());
    for (service, method) in [
        ("structureSettings", "CharacterGetServices"),
        ("structureGuests", "GetGuests"),
        ("structureHangarViewMgr", "GetMyHangarViewState"),
    ] {
        for id in [Value::None, Value::Int(i64::MAX)] {
            let args = [id];
            let args = if service == "structureHangarViewMgr" { &[][..] } else { &args[..] };
            let call = Call { service, method, args, kwargs: &kwargs, bound: false, bind_params: None };
            let reply = dispatch(&mut client, &call).await.unwrap();
            let decoded = marshal::loads(&marshal::dumps(&reply)).unwrap();
            if service == "structureHangarViewMgr" {
                let Value::Instance(class, fields) = decoded else { panic!("expected KeyVal") };
                assert_eq!(*class, Value::str("utillib.KeyVal"));
                assert_eq!(fields.get("damageState"), Some(&Value::tuple([Value::Float(1.0), Value::Float(1.0), Value::Float(1.0)])));
            } else {
                assert_eq!(decoded, Value::Dict(vec![]));
            }
            assert_eq!(client.session.sid, sid);
            assert_eq!(client.session.attributes().map(|(key, value)| (key.to_owned(), value.clone())).collect::<Vec<_>>(), before);
            assert!(client.session.int("structureid").is_none());
        }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn map_insurgency_fixed_dispatch_roundtrips_empty_list() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    let call = Call {
        service: "map",
        method: "GetInsurgencySystem",
        args: &[],
        kwargs: &kwargs,
        bound: false,
        bind_params: None,
    };
    let reply = dispatch(&mut client, &call).await.unwrap();
    assert_eq!(reply, Value::list([]));
    assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::list([]));

    let unknown = Call {
        service: "map",
        method: "UnknownCompatibilityMethod",
        args: &[],
        kwargs: &kwargs,
        bound: false,
        bind_params: None,
    };
    assert_eq!(dispatch(&mut client, &unknown).await.unwrap(), Value::None);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn item_locking_dispatch_returns_empty_collections() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    let location_args = [Value::Int(1)];
    for (method, args, expected) in [
        ("GetItemsByLocation", &location_args[..], Value::Dict(vec![])),
        ("GetLockedItemLocations", &[][..], Value::reduce("__builtin__.set", vec![Value::list([])], None)),
    ] {
        let call = Call { service: "itemLocking", method, args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn item_fixed_dispatch_roundtrips_encoded_decode_responses() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);

    let item_id = [Value::Int(1)];
    let decode = Call { service: "encodedItems", method: "DecodeItem", args: &item_id, kwargs: &kwargs, bound: false, bind_params: None };
    let decode_error = dispatch(&mut client, &decode).await.unwrap_err();
    let CallError::User { msg, dict } = decode_error else { panic!("expected CustomNotify refusal") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::encoded_items::DECODE_UNAVAILABLE))]);
    let wire_error = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&wire_error)).unwrap(), wire_error);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn character_selection_fixed_dispatch_without_selected_character() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 0).await;
    client.session.apply(vec![("charid", Value::None)]);
    assert_eq!(client.session.int("charid"), None);
    let kwargs = Value::Dict(vec![]);
    for method in ["GetQAStarterSystemIDs", "GetCharacterInfo"] {
        let call = Call { service: "charUnboundMgr", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::list([]));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::list([]));
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn corporate_mining_ledger_fixed_dispatch_returns_empty_lists() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    let stale_structure_args = [Value::Int(1)];
    for (method, args) in [
        ("GetObserversWithMiningEvents", &[][..]),
        ("GetObserverLedger", &stale_structure_args[..]),
    ] {
        let call = Call { service: "corpMiningLedger", method, args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::list([]));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::list([]));
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn mobile_binding_callback_dispatch_returns_none() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    for aeid in ["fictional-aeid-one", "fictional-aeid-two"] {
        let args = [Value::str(aeid)];
        let call = Call { service: "mobileCollectMgr", method: "MobileBindSucceed", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::None);
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn closed_structure_dispatch_preserves_fixture_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = client(test_db.clone(), owner.character).await;
        let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let sid = client.session.sid;
        let outbox = client.outbox.len();
        let pilots = client.world.online_pilots();
        for item in [own_item, foreign_item] {
            let id = Value::Int(item);
            let system = Value::Int(1);
            let text = Value::unicode("test");
            let items = Value::list([id.clone()]);
            let empty = Value::Dict(vec![]);
            let undock_kw = Value::dict([("ignoreContraband", Value::Bool(false))]);
            let destination_kw = Value::dict([("destinationID", id.clone())]);
            let cases: Vec<(&str, &str, Vec<Value>, &Value, Option<Value>)> = vec![
                ("structureDirectory", "GetMyCharacterStructures", vec![], &empty, Some(Value::Dict(vec![]))),
                ("structureDirectory", "GetMyCorporationStructures", vec![], &empty, Some(Value::Dict(vec![]))),
                ("structureDirectory", "GetStructureMapData", vec![system.clone()], &empty, Some(Value::list([]))),
                ("structureDirectory", "GetMyDockableStructures", vec![system.clone()], &empty, Some(Value::list([]))),
                ("structureDirectory", "GetSolarSystemsWithBeacons", vec![], &empty, Some(Value::list([]))),
                ("structureDirectory", "GetNearbyJumpBridges", vec![], &empty, Some(Value::list([]))),
                ("structureDirectory", "GetJumpBridgesWithMyAccess", vec![], &empty, Some(Value::tuple([Value::list([]), Value::list([]), Value::list([])]))),
                ("structureDirectory", "GetStructureDescription", vec![id.clone()], &empty, Some(Value::unicode(""))),
                ("structureDirectory", "SetStructureDescription", vec![id.clone(), text.clone()], &empty, None),
                ("structureSettings", "CharacterHasSetting", vec![id.clone(), Value::Int(1)], &empty, Some(Value::Bool(false))),
                ("structureSettings", "CharacterHasService", vec![id.clone(), Value::Int(1)], &empty, Some(Value::Bool(false))),
                ("structureSettings", "CharacterGetService", vec![id.clone(), Value::Int(1)], &empty, None),
                ("structureSettings", "CharacterCheckService", vec![id.clone(), Value::Int(1)], &empty, None),
                ("structureDeployment", "RenameStructure", vec![id.clone(), text.clone()], &empty, None),
                ("structureDeployment", "Unanchor", vec![id.clone()], &empty, None),
                ("structureDeployment", "CancelUnanchor", vec![id.clone()], &empty, None),
                ("structureDeployment", "Anchor", vec![id.clone(), Value::Float(0.0), Value::Float(0.0), Value::Float(0.0), Value::Int(1), text.clone(), text.clone(), Value::Int(1), Value::Int(1), Value::Dict(vec![])], &empty, None),
                ("structureDeliveries", "Deliver", vec![id.clone(), Value::Int(owner.character), items.clone()], &empty, None),
                ("structureControl", "TakeControl", vec![id.clone()], &empty, None),
                ("structureControl", "ReleaseControl", vec![], &empty, None),
                ("structureControl", "GetStructurePilot", vec![id.clone()], &empty, Some(Value::None)),
                ("structureJumpBridgeMgr", "GetJbStructureDestination", vec![id.clone()], &empty, Some(Value::None)),
                ("structureJumpBridgeMgr", "GetLinkedStructure", vec![id.clone()], &empty, Some(Value::None)),
                ("structureDocking", "Undock", vec![id.clone(), id.clone()], &undock_kw, None),
                ("structureDocking", "Dock", vec![id.clone(), id.clone()], &empty, None),
                ("structureVulnerability", "UpdateReinforceTimesFromClient", vec![Value::list([]), Value::Int(1), Value::Int(1)], &empty, None),
                ("structureCargoDelivery", "DropOffItems", vec![items.clone(), id.clone(), id.clone()], &empty, None),
                ("structureAssetSafety", "MoveEjectTimeGM", vec![id.clone(), Value::Int(1)], &empty, None),
                ("structureAssetSafety", "GetStructuresICanDeliverTo", vec![system.clone()], &empty, Some(Value::tuple([Value::list([]), Value::None]))),
                ("structureAssetSafety", "MoveSafetyWrapToStructure", vec![id.clone(), system.clone()], &destination_kw, None),
                ("structureAssetSafety", "MovePersonalAssetsToSafety", vec![system.clone(), id.clone()], &empty, None),
                ("structureAssetSafety", "MoveCorpAssetsToSafety", vec![system.clone(), id.clone()], &empty, None),
                ("structureAssetSafety", "GetWrapNames", vec![items], &empty, Some(Value::Dict(vec![]))),
                ("structureCynoBeaconMgr", "CmdBridgeToStructureBeacon", vec![id.clone(), system.clone()], &empty, None),
                ("structureCynoBeaconMgr", "CmdGroupJumpToStructureBeacon", vec![id.clone(), system], &empty, None),
                ("structureProfiles", "GetProfiles", vec![], &empty, Some(Value::list([]))),
                ("structureProfiles", "CreateProfile", vec![text.clone(), text], &empty, None),
            ];
            for (service, method, args, kwargs, expected) in cases {
                let call = Call { service, method, args: &args, kwargs, bound: false, bind_params: None };
                match (dispatch(&mut client, &call).await, expected) {
                    (Ok(value), Some(expected)) => {
                        assert_eq!(value, expected, "{service}.{method}");
                        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
                    }
                    (Err(CallError::User { msg, dict }), None) => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::structures::UNAVAILABLE))]);
                        let wire = crate::client::user_error(&msg, dict);
                        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                    }
                    (other, _) => panic!("unexpected result for {service}.{method}: {other:?}"),
                }
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
                assert_eq!(client.session.sid, sid);
                assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                assert!(client.session.int("structureid").is_none());
                assert_eq!(client.outbox.len(), outbox);
                assert_eq!(client.world.online_pilots(), pilots);
                assert!(client.world.ball(1, item).await.is_none());
                let mut byte = [0];
                assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            }
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn moon_boundary_dispatch_preserves_fixture_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = client(test_db.clone(), owner.character).await;
        let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let sid = client.session.sid;
        let outbox = client.outbox.len();
        let pilots = client.world.online_pilots();
        let kwargs = Value::Dict(vec![]);
        for item in [own_item, foreign_item] {
            let id = Value::Int(item);
            let system = Value::Int(1);
            let cases: Vec<(&str, &str, Vec<Value>, Option<Value>, &str)> = vec![
                ("moonExtractions", "GetExtractionAndEventIDFromStructureID", vec![], Some(Value::tuple([Value::None, Value::None])), ""),
                ("moonExtractions", "GetExtractionsForCorp", vec![], Some(Value::list([])), ""),
                ("moonExtractions", "GetMoonResourcesForStructure", vec![], Some(Value::Dict(vec![])), ""),
                ("moonExtractions", "StartNewExtraction", vec![id.clone(), Value::Int(1), Value::Bool(false)], None, infinity_core::messages::mining::MOON_EXTRACTION_UNAVAILABLE),
                ("moonExtractions", "FractureChunkWithStructure", vec![], None, infinity_core::messages::mining::MOON_EXTRACTION_UNAVAILABLE),
                ("moonExtractions", "CancelExtraction", vec![], None, infinity_core::messages::mining::MOON_EXTRACTION_UNAVAILABLE),
                ("structureCompressionMgr", "DecompressGasInStructure", vec![id.clone()], None, infinity_core::messages::mining::STRUCTURE_COMPRESSION_UNAVAILABLE),
                ("structureCompressionMgr", "GetMyGasDecompressionEfficiency", vec![], None, infinity_core::messages::mining::STRUCTURE_COMPRESSION_UNAVAILABLE),
                ("structureCompressionMgr", "CompressItemInStructure", vec![id.clone()], None, infinity_core::messages::mining::STRUCTURE_COMPRESSION_UNAVAILABLE),
                ("autoMoonMining", "RequestMiningDetails", vec![system.clone(), id.clone()], None, infinity_core::messages::mining::AUTO_MOON_MINING_UNAVAILABLE),
                ("autoMoonMining", "RequestMiningCycleOutput", vec![system, id], Some(Value::Dict(vec![])), ""),
            ];
            for (service, method, args, expected, message) in cases {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                match (dispatch(&mut client, &call).await, expected) {
                    (Ok(value), Some(expected)) => {
                        assert_eq!(value, expected, "{service}.{method}");
                        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
                    }
                    (Err(CallError::User { msg, dict }), None) => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(message))]);
                        let wire = crate::client::user_error(&msg, dict);
                        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                    }
                    (other, _) => panic!("unexpected result for {service}.{method}: {other:?}"),
                }
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
                assert_eq!(client.session.sid, sid);
                assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                assert_eq!(client.outbox.len(), outbox);
                assert_eq!(client.world.online_pilots(), pilots);
                assert!(client.world.ball(1, item).await.is_none());
                let mut byte = [0];
                assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            }
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn starbase_boundary_dispatch_preserves_fixture_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = client(test_db.clone(), owner.character).await;
        let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let sid = client.session.sid;
        let outbox = client.outbox.len();
        let pilots = client.world.online_pilots();
        let kwargs = Value::Dict(vec![]);
        for item in [own_item, foreign_item] {
            for method in [
                "GetControlTowers", "GetTowerSentrySettings", "GetTowerNotificationSettings",
                "GetStarbasePermissions", "GetMoonForTower", "GetConsumptionFactor",
            ] {
                let args = if method == "GetControlTowers" { vec![] } else { vec![Value::Int(item)] };
                let call = Call { service: "posMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                match dispatch(&mut client, &call).await {
                    Ok(value) if method == "GetControlTowers" => {
                        assert_eq!(value, Value::list([]));
                        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
                    }
                    Err(CallError::User { msg, dict }) if method != "GetControlTowers" => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::structures::STARBASE_UNAVAILABLE))]);
                        let wire = crate::client::user_error(&msg, dict);
                        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                    }
                    other => panic!("unexpected result for posMgr.{method}: {other:?}"),
                }
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
                assert_eq!(client.session.sid, sid);
                assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                assert_eq!(client.outbox.len(), outbox);
                assert_eq!(client.world.online_pilots(), pilots);
                assert!(client.world.ball(1, item).await.is_none());
                let mut byte = [0];
                assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            }
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
