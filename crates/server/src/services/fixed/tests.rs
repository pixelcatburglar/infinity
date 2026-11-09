use super::*;
mod account;
mod ess;
mod fighter_boundaries;
mod keeper;
mod operations;
mod travel;
mod survey_pvp;
mod platform_login;
mod platform_tools;
mod support;
mod season;
mod paid_platform;
mod tourney;

#[test]
fn cyno_jammer_state_is_none() {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "sovDashboard", method: "GetCynoJammerState", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(root(&call).unwrap().unwrap(), Value::None);
}

#[test]
fn login_campaign_fixed_answers_do_not_match_other_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("otherService", "claim_reward"),
        ("loginCampaignManager", "UnknownMethod"),
        ("seasonalLoginCampaignManager", "claim_reward"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn character_selection_fixed_answers_do_not_match_other_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("otherService", "GetQAStarterSystemIDs"),
        ("otherService", "GetCharacterInfo"),
        ("charUnboundMgr", "UnknownMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn corporate_mining_ledger_fixed_answers_do_not_match_other_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("otherService", "GetObserversWithMiningEvents"),
        ("otherService", "GetObserverLedger"),
        ("corpMiningLedger", "UnknownMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn mobile_binding_callback_does_not_match_other_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("otherService", "MobileBindSucceed"),
        ("mobileCollectMgr", "UnknownMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn qa_mission_selection_matches_only_its_exact_call() {
    let kwargs = Value::None;
    for (service, method) in [("otherService", "GetPlausibleMissionIDs"), ("agentMgr", "OtherMethod")] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn encoded_item_answer_matches_only_exact_call() {
    let kwargs = Value::None;
    for (service, method) in [("otherService", "DecodeItem"), ("encodedItems", "OtherMethod")] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}


#[test]
fn warfare_fixed_answers_only_claim_the_named_calls() {
    let kwargs = Value::None;
    for (service, method) in [
        ("map", "unknown"), ("facWarMgr", "unknown"),
        ("shipCasterLandingPadMgr", "unknown"), ("shipCasterLauncherMgr", "unknown"),
        ("other", "GetFactionLandingPads"), ("other", "GetFacWarZoneInfo"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(universe(&call).is_none());
    }
}

#[test]
fn rw_nearest_stations_are_empty_and_only_exact_calls_are_claimed() {
    let kwargs = Value::Dict(vec![]);
    for system in [Value::Int(1), Value::Int(2), Value::None] {
        let args = [system];
        let call = Call { service: "RWManager", method: "get_closest_rw_stations", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = social(&call).unwrap().unwrap();
        assert_eq!(reply, Value::Dict(vec![]));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::Dict(vec![]));
    }
    for (service, method) in [("RWManager", "unknown"), ("other", "get_closest_rw_stations")] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(social(&call).is_none());
    }
    let call = Call { service: "RWManager", method: "solarsystem_contains_rw_instances", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(social(&call).unwrap().unwrap(), Value::Bool(false));
}

#[test]
fn structure_empty_reads_preserve_client_shapes_and_exact_ownership() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("structureSettings", "CharacterGetServices"),
        ("structureGuests", "GetGuests"),
    ] {
        for id in [Value::None, Value::Int(i64::MAX)] {
            let args = [id];
            let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let reply = station(&call).unwrap().unwrap();
            let Value::Dict(entries) = marshal::loads(&marshal::dumps(&reply)).unwrap() else { panic!("expected dictionary") };
            assert!(entries.is_empty());
        }
    }
    let call = Call { service: "structureHangarViewMgr", method: "GetMyHangarViewState", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let reply = station(&call).unwrap().unwrap();
    let Value::Instance(class, fields) = marshal::loads(&marshal::dumps(&reply)).unwrap() else { panic!("expected KeyVal") };
    assert_eq!(*class, Value::str("utillib.KeyVal"));
    assert!(matches!(fields.as_ref(), Value::Dict(entries) if entries.len() == 7));
    for name in ["upkeepState", "operatingState", "timerProgress", "timerEndAt"] {
        assert_eq!(fields.get(name), Some(&Value::None));
    }
    for name in ["timerIsProgressing", "timerIsPaused"] {
        assert_eq!(fields.get(name), Some(&Value::Bool(false)));
    }
    let Some(Value::Tuple(damage)) = fields.get("damageState") else { panic!("expected damage tuple") };
    assert_eq!(damage.len(), 3);
    for index in 0..3 {
        assert_eq!(damage[index], Value::Float(1.0));
    }
    for (service, method) in [
        ("structureSettings", "UnknownMethod"), ("structureGuests", "UnknownMethod"),
        ("structureHangarViewMgr", "UnknownMethod"), ("other", "CharacterGetServices"),
        ("other", "GetGuests"), ("other", "GetMyHangarViewState"),
        ("structureSettings", "GetGuests"), ("structureGuests", "CharacterGetServices"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(station(&call).is_none());
    }
}

#[test]
fn closed_structure_calls_preserve_wire_shapes_and_refuse_actions() {
    let kwargs = Value::Dict(vec![]);
    for (service, method, expected) in [
        ("structureSettings", "CharacterHasSetting", Value::Bool(false)),
        ("structureSettings", "CharacterHasService", Value::Bool(false)),
        ("structureControl", "GetStructurePilot", Value::None),
        ("structureJumpBridgeMgr", "GetJbStructureDestination", Value::None),
        ("structureJumpBridgeMgr", "GetLinkedStructure", Value::None),
        ("structureAssetSafety", "GetStructuresICanDeliverTo", Value::tuple([Value::list([]), Value::None])),
        ("structureAssetSafety", "GetWrapNames", Value::Dict(vec![])),
        ("structureProfiles", "GetProfiles", Value::list([])),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let reply = root(&call).unwrap().unwrap();
        assert_eq!(reply, expected, "{service}.{method}");
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
    }
    for (service, method) in [
        ("structureSettings", "CharacterGetService"), ("structureSettings", "CharacterCheckService"),
        ("structureDeployment", "RenameStructure"), ("structureDeployment", "Unanchor"),
        ("structureDeployment", "CancelUnanchor"), ("structureDeployment", "Anchor"),
        ("structureDeliveries", "Deliver"), ("structureControl", "TakeControl"),
        ("structureControl", "ReleaseControl"), ("structureDocking", "Undock"),
        ("structureDocking", "Dock"), ("structureVulnerability", "UpdateReinforceTimesFromClient"),
        ("structureCargoDelivery", "DropOffItems"), ("structureAssetSafety", "MoveEjectTimeGM"),
        ("structureAssetSafety", "MoveSafetyWrapToStructure"), ("structureAssetSafety", "MovePersonalAssetsToSafety"),
        ("structureAssetSafety", "MoveCorpAssetsToSafety"), ("structureCynoBeaconMgr", "CmdBridgeToStructureBeacon"),
        ("structureCynoBeaconMgr", "CmdGroupJumpToStructureBeacon"), ("structureProfiles", "CreateProfile"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let super::super::CallError::User { msg, dict } = root(&call).unwrap().unwrap_err() else { panic!("expected refusal for {service}.{method}") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::structures::UNAVAILABLE))]);
        let wire = crate::client::user_error(&msg, dict);
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
    }
    for (service, method) in [("structureProfiles", "DeleteProfile"), ("structureControl", "BoardStructure"), ("structureCynoBeaconMgr", "CmdJumpToStructureBeacon"), ("structureSettings", "UnknownMethod"), ("other", "GetProfiles")] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn moon_and_structure_compression_contract_has_exact_wire_shapes() {
    let kwargs = Value::Dict(vec![]);
    for id in [1, i64::MAX] {
        let args = [Value::Int(id)];
        for (service, method, expected) in [
            ("moonExtractions", "GetExtractionAndEventIDFromStructureID", Value::tuple([Value::None, Value::None])),
            ("moonExtractions", "GetExtractionsForCorp", Value::list([])),
            ("moonExtractions", "GetMoonResourcesForStructure", Value::Dict(vec![])),
            ("autoMoonMining", "RequestMiningCycleOutput", Value::Dict(vec![])),
        ] {
            let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let reply = root(&call).unwrap().unwrap();
            assert_eq!(reply, expected, "{service}.{method}");
            assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
        }
        for (service, method, message) in [
            ("moonExtractions", "StartNewExtraction", infinity_core::messages::mining::MOON_EXTRACTION_UNAVAILABLE),
            ("moonExtractions", "FractureChunkWithStructure", infinity_core::messages::mining::MOON_EXTRACTION_UNAVAILABLE),
            ("moonExtractions", "CancelExtraction", infinity_core::messages::mining::MOON_EXTRACTION_UNAVAILABLE),
            ("structureCompressionMgr", "DecompressGasInStructure", infinity_core::messages::mining::STRUCTURE_COMPRESSION_UNAVAILABLE),
            ("structureCompressionMgr", "GetMyGasDecompressionEfficiency", infinity_core::messages::mining::STRUCTURE_COMPRESSION_UNAVAILABLE),
            ("structureCompressionMgr", "CompressItemInStructure", infinity_core::messages::mining::STRUCTURE_COMPRESSION_UNAVAILABLE),
            ("autoMoonMining", "RequestMiningDetails", infinity_core::messages::mining::AUTO_MOON_MINING_UNAVAILABLE),
        ] {
            let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let super::super::CallError::User { msg, dict } = root(&call).unwrap().unwrap_err() else { panic!("expected refusal for {service}.{method}") };
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(message))]);
            let wire = crate::client::user_error(&msg, dict);
            assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        }
    }
    for (service, method) in [
        ("moonExtractions", "UnknownMethod"), ("structureCompressionMgr", "UnknownMethod"),
        ("autoMoonMining", "UnknownMethod"), ("other", "RequestMiningCycleOutput"),
        ("moonExtractions", "RequestMiningDetails"), ("autoMoonMining", "CancelExtraction"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn starbase_boundary_has_exact_ownership_and_wire_shapes() {
    let kwargs = Value::Dict(vec![]);
    let methods = [
        "GetControlTowers", "GetTowerSentrySettings", "GetTowerNotificationSettings",
        "GetStarbasePermissions", "GetMoonForTower", "GetConsumptionFactor",
    ];
    for method in methods {
        let args = if method == "GetControlTowers" { vec![] } else { vec![Value::Int(1)] };
        let call = Call { service: "posMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        if method == "GetControlTowers" {
            let value = root(&call).unwrap().unwrap();
            assert_eq!(value, Value::list([]));
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
        } else {
            let super::super::CallError::User { msg, dict } = root(&call).unwrap().unwrap_err() else { panic!("expected refusal for {method}") };
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::structures::STARBASE_UNAVAILABLE))]);
            let wire = crate::client::user_error(&msg, dict);
            assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        }
        let other = Call { service: "other", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&other).is_none(), "{method}");
    }
    let unknown = Call { service: "posMgr", method: "UnknownMethod", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(root(&unknown).is_none());
}
