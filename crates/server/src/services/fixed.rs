//! Constant replies for unmodelled remote services.
//! Mirrors client remote-service calls; no database or state is accessed.
//! Stateful and argument-dependent services are handled by their feature modules;
//! personal contact label management and corporation item lockdown votes are unsupported.
//! Encoded-item decoding is refused until media decoding is modelled. Item-locking
//! queries retain their empty-dictionary and mutable-set compatibility replies.
//! `eve/client/script/ui/services/ccSvc.py` uses an empty QA starter-system list
//! to retain ordinary allocation; QA overrides are unavailable.
//! `eve/client/script/ui/shared/neocom/petitionWindow.py` receives no character
//! choices before selection; this does not implement the rest of petitions.
//! Corporate mining observers and structure ledger history are not modelled;
//! their queries return empty lists.
//! External phone binding, binding state and rewards are unsupported; the
//! binding-success callback does not establish binding state.
//! Player structures are unsupported, so the character structure directory is empty.
//! Single-track login campaigns are closed; seasonal login campaigns remain separate.
//! `loginrewards/client/loginCampaignService.py` receives no campaign or claim.
//! Inventory configuration and item-price writes/reads are explicitly refused;
//! crystal damage is handled by the inventory feature module.

use super::{notify_error, rowset, Call, CallResult};
use super::universe::{WARZONES, DYNAMIC_RESOURCE_MAX_OUTPUT};
use marshal::Value;
/// Client paperdoll.State.no_recustomization.
const PAPERDOLL_NO_RECUSTOMIZATION: i64 = 0;

/// CPython 2.7 code flags: CO_OPTIMIZED | CO_NEWLOCALS | CO_NOFREE.
const SETTINGS_CODE_FLAGS: i32 = 0x43;
/// CPython 2.7 bytecode for BUILD_MAP 0; RETURN_VALUE.
const SETTINGS_CODE_BYTES: &[u8] = &[0x69, 0x00, 0x00, 0x53];
/// CPython 2.7 `marshal.dumps(code)` for `def SettingsInfo(): return {}`.
/// carbonui settingsSvc.UpdateSettingsStatistics loads it into a function and calls it.
fn settings_info_code() -> Vec<u8> {
    fn i32le(out: &mut Vec<u8>, v: i32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn s(out: &mut Vec<u8>, b: &[u8]) {
        out.push(b's');
        i32le(out, b.len() as i32);
        out.extend_from_slice(b);
    }
    fn empty_tuple(out: &mut Vec<u8>) {
        out.push(b'(');
        i32le(out, 0);
    }
    let mut o = vec![b'c'];
    i32le(&mut o, 0); // argcount
    i32le(&mut o, 0); // nlocals
    i32le(&mut o, 1); // stacksize
    i32le(&mut o, SETTINGS_CODE_FLAGS);
    s(&mut o, SETTINGS_CODE_BYTES);
    o.push(b'(');
    i32le(&mut o, 1);
    o.push(b'N'); // consts = (None,)
    empty_tuple(&mut o); // names
    empty_tuple(&mut o); // varnames
    empty_tuple(&mut o); // freevars
    empty_tuple(&mut o); // cellvars
    s(&mut o, b"infinity-emu"); // filename
    s(&mut o, b"SettingsInfo"); // name
    i32le(&mut o, 1); // firstlineno
    s(&mut o, b""); // lnotab
    o
}
/// Client kiring.common.const.ANTI_ADDICTION_PASS_CODE.
const ANTI_ADDICTION_PASS_CODE: i64 = 201;

pub(super) fn universe(call: &Call<'_>) -> Option<CallResult> {
    let history_header = ["solarSystemID", "value1", "value2", "value3"];
    Some(Ok(match (call.service, call.method) {
        ("map", "GetMyExtraMapInfo") => Value::None,
        ("map", "GetMyExtraMapInfoAgents") => rowset(&["fromID", "rank"], vec![]),
        ("map", "GetDeadspaceComplexMap") | ("map", "GetDeadspaceAgentsMap") => Value::None,
        ("map", "GetBeaconCount") => Value::Dict(vec![]),
        ("map", "GetIndustryJobsOverLast") => rowset(&history_header, vec![]),
        ("map", "GetInsurgencySystem") => Value::list([]),
        ("map", "GetAllRoamingWeatherSystems") => Value::Dict(vec![]),
        ("map", "GetTriglavianMinorVictorySystems") | ("map", "GetEdencomMinorVictorySystems") | ("map", "GetEdencomFortressSystems") => {
            Value::list([])
        }
        ("stationSvc", "GetAllianceSystems") => Value::list([]),
        // (jumpBridgeGates, hasAccessTo, hasNoAccessTo)
        ("structureDirectory", "GetStructureMapData") => Value::list([]),
        // GetMarketDockableLocationsInRegion iterates the dict and reads each entry's
        // services; player structures are unsupported.
        ("structureDirectory", "GetMyCharacterStructures") => Value::Dict(vec![]),
        // SKIN licences the character has activated: none yet.
        ("shipCosmeticsMgr", "GetLicencedSkins") | ("shipCosmeticsMgr", "GetLicencedSkinsForShipType") => Value::list([]),
        ("structureDirectory", "GetJumpBridgesWithMyAccess") => Value::tuple([Value::list([]), Value::list([]), Value::list([])]),
        ("dynamicResourceCacheMgr", "GetDBSMapData") => Value::Dict(vec![]),
        // Bounty output range and its equilibrium (mapFilterDynamicBounty reads all three).
        ("dynamicResourceCacheMgr", "GetDynamicResourceSettings") => Value::dict([
            ("minOutput", Value::Float(0.0)),
            ("maxOutput", Value::Float(DYNAMIC_RESOURCE_MAX_OUTPUT)),
            ("equilibriumValue", Value::Float(1.0)),
        ]),
        ("structureDirectory", "GetSolarSystemsWithBeacons") => Value::list([]),
        // {solarSystemID: [instances] or count}; the Agency's anomaly trackers iterate these
        // as dicts and add the current system's sites from the sensor overlay themselves.
        ("dungeonInstanceCacheMgr", _) => Value::Dict(vec![]),
        ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates") | ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached") => {
            Value::Dict(WARZONES.iter().map(|(id, _)| (Value::Int(*id), Value::Dict(vec![]))).collect())
        }
        // (pilots by solarSystemID, docked pilots by solarSystemID, divisor)
        ("machoNet", "GetClusterGameStatisticsForClient") => Value::tuple([Value::Dict(vec![]), Value::Dict(vec![]), Value::Int(1)]),
        // {warzoneID: {solarSystemID: occupationState}}: no system is contested yet, but every
        // warzone must be present (WarzoneStatusGauge indexes it by the faction's warzone).
        ("fwWarzoneSolarsystem", "GetAllWarzones") => {
            Value::Dict(WARZONES.iter().map(|(id, factions)| (Value::Int(*id), Value::list(factions.iter().map(|f| Value::Int(*f))))).collect())
        }

        // ---- corporations ----
        // eve/client/script/ui/services/corporation/itemLocking.py:59 caches an itemID-to-row mapping for membership checks.
        ("itemLocking", "GetItemsByLocation") => Value::Dict(vec![]),
        // eve/client/script/ui/services/corporation/itemLocking.py:73-77 caches and mutates the set of locations containing locked items.
        ("itemLocking", "GetLockedItemLocations") => Value::reduce("__builtin__.set", vec![Value::list([])], None),
        ("corpmgr", "GetAssetInventoryForLocation") | ("corpmgr", "SearchAssets") => Value::list([]),
        // A player character's corporation, or an NPC's (agents) from the SDE.
        ("corpRegistry", "GetAllianceApplications") => Value::Dict(vec![]),
        ("corpRegistry", "GetPendingAutoKicks") => Value::list([]),
        ("corpRegistry", "GetStructureReinforceDefault") => Value::None,
        ("corpRegistry", "DoesMyCorpAcceptStructures") | ("corpRegistry", "DoesCorpRestrictCorpMails") => Value::Bool(false),
        ("allianceRegistry", "GetDaysInAlliance") => Value::Int(0),
        ("allianceRegistry", "GetAllianceMembersOlderThan") => Value::list([]),

        // ---- wallet ----
        ("rewardMgr", "GetRewardLPLogs") => Value::list([]),
        ("account", "GetJournal") | ("account", "GetJournalForAccounts") | ("account", "GetTransactions") => Value::list([]),
        ("account", "GetWalletDivisionsInfo") => Value::list([]),
        ("lookupSvc", _) => Value::list([]),

        // ---- mail, notifications, contracts ----
        // Client-side settings blobs (yaml); nothing reads them back yet.
        ("charMgr", "SaveCharacterSetting") => Value::None,

        ("map", "GetIncursionGlobalReport") => Value::list([]),
        // md5(str([])): the client's cached (empty) flagged-site list is current.
        ("browserLockdownSvc", "GetFlaggedSitesHash") => Value::str("d751713988987e9331980363e24189ce"),
        ("browserLockdownSvc", "GetFlaggedSitesList") => Value::list([]),
        ("charMgr", "GetPrivateInfoOnCorpChange") => Value::None,

        // ---- agency / job board ----
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails") => Value::list([]),
        // (factionID, directFactionID, corpFactionID): not enlisted.
        ("fwCharacterEnlistmentMgr", "GetMyEnlistment") => Value::tuple([Value::None, Value::None, Value::None]),
        // FILETIME the direct-enlistment cooldown ends; never enlisted, so none.
        ("fwCharacterEnlistmentMgr", "GetMyDirectEnlistmentCooldownTimestamp") => Value::Int(0),
        // Not enlisted in a militia: no ranks.
        ("facWarMgr", "GetMyCharacterRankOverview") => Value::list([]),
        // (medals, medalInfo): medals come from player corporations; medalInfo is only
        // Filter()ed per received medal.
        ("corporationSvc", "GetMedalsReceived") => Value::tuple([Value::list([]), Value::list([])]),
        // An NPC corporation's market supply and demand (typeID, sellPrice, buyPrice...): none traded.
        ("corporationSvc", "GetCorpInfo") => Value::list([]),
        ("repairableShipMgr", "GetServiceLifeDamage") => Value::Float(0.0),
        ("monolithTaskTrackerMgr", "UpdateClientAchievmentsAndCounters") => Value::None,
        // The adaptation tree answers through notifications; there are no trees to send.
        ("shipAdaptionMgr", "fetch_tree_data") | ("shipAdaptionMgr", "fetch_missions") => Value::None,
        _ => return None,
    }))
}

pub(super) fn station(call: &Call<'_>) -> Option<CallResult> {
    let int = Value::Int;
    let empty_dict = || Value::Dict(vec![]);
    Some(Ok(match (call.service, call.method) {
        ("seasonManager", "get_season_data_for_character") => Value::None,
        // (jumpActivation, jumpFatigue, lastUpdated)
        ("petitioner", "GetUnreadMessages") => Value::list([]),
        // No external help centre: the help window keeps the in-game petition flow.
        ("petitioner", "IsZendeskEnabled") | ("petitioner", "IsZendeskSwapEnabled") => Value::Bool(false),
        ("shipKillCounter", "GetItemKillCountPlayer") => int(0),
        ("shipCosmeticsMgr", "GetEnabledCosmetics") => empty_dict(),
        ("keeper", "GetCurrentDungeonForCharacter") => Value::None,
        ("scanMgr", "SignalTrackerRegister") => Value::None,
        ("monolithTaskTrackerMgr", "HasEverWarped") => Value::Bool(true),
        ("essMgr", "GetDataForClientSolarSystem") => Value::None,
        // (solarSystemID, campaignSnapshot): no insurgency here. The system must match the
        // session's or the client asks again.
        ("userSvc", "GetMultiCharactersTrainingSlots") => empty_dict(),
        ("securityMgr", "get_modified_systems") => empty_dict(),
        ("monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount") => {
            Value::dict([("completedDict", empty_dict()), ("eventDict", empty_dict())])
        }
        // (fightersInTubes, fightersInSpace, abilityStates)
        ("fighterMgr", "GetFightersForShip") => Value::tuple([Value::list([]), Value::list([]), empty_dict()]),
        // Placebo-signed CPython code object: (marshalled code, 0)
        ("charMgr", "GetSettingsInfo") => Value::tuple([Value::str(settings_info_code()), int(0)]),
        ("charMgr", "LogSettings") => Value::None,
        ("charMgr", "GetCharacterSettings") => empty_dict(),
        // addressbookService.GetContactLabels caches this mapping and reads its values.
        ("charMgr", "GetLabels") => empty_dict(),
        ("ProjectDiscovery", "initialize_tutorial_status") => Value::Bool(true),
        // Analysis Kredits (the LP store's Project Discovery currency).
        ("ProjectDiscovery", "get_player_analysis_kredits") => int(0),
        // Store telemetry/refresh requests; the live server answers with pushed data we don't model.
        ("monetizationMgr", "CallUserRequested") | ("exchangeShopMgr", "CallExchangeShopOpened") => Value::None,
        // Moving ISK into the amplification matrix isn't modelled: refuse rather than let the
        // client report a deposit that never happened.
        ("amplificationMatrixSolarsystemMgr", "TransferCash") => {
            return Some(Err(super::CallError::User {
                msg: crate::messages::MSG_CUSTOM_NOTIFY.into(),
                dict: vec![("notify".into(), Value::unicode(crate::messages::AMPLIFICATION_MATRIX_UNAVAILABLE))],
            }))
        }
        ("milestoneMgr", "ProcessCharacterLogon") => Value::None,
        ("structureDirectory", "GetMyDockableStructures") => Value::list([]),
        ("activityMgr", "GetActivityBadgeStatuses") => Value::list([]),
        // The activities window's auto-popup setting (0: open it at login), kept per character.
        ("activityMgr", "ActivitySeen") => Value::None,
        ("operationsManager", "can_character_play_the_tutorial") => Value::Bool(false),
        ("insurgencySolarsystem", "GetAllVisibleCampaigns") => Value::list([]),
        ("battlepassSeasonMgr", "CallSeasonDataRequested") => Value::None,
        ("shipAdaptionMgr", "fetch_missions") => Value::None,
        ("clientStatLogger", _) => Value::None,
        // [ownerID, itemID, operationID, stationTypeID]
        _ => return None,
    }))
}

/// Fixed answers that originally preceded feature dispatch.
pub(super) fn root(call: &Call<'_>) -> Option<CallResult> {
    let s = Value::str;
    Some(match (call.service, call.method) {
        ("machoNet", "GetServiceInfo") => Ok(Value::Dict(vec![])),
        ("air_npe", "is_air_npe_enabled") => Ok(Value::Bool(false)),

        // menuFunctions.py:DecryptItem cannot open media without a supported source; refuse without consuming the item.
        ("encodedItems", "DecodeItem") => Err(notify_error(crate::messages::encoded_items::DECODE_UNAVAILABLE)),
        // invControllers.py:BaseInvContainer.__AddItem reads this root mask.
        ("invInventory", "ALSCConfigGet") => Ok(Value::Int(0)),
        // ConfigureALSC/menuFunctions.py writes config; persistence is unsupported.
        ("invInventory", "ALSCConfigSet") => Err(notify_error(crate::messages::inventory::ALSC_CONFIG_UNSUPPORTED)),
        // invCache.py:AcceptPossibleRemovalTax requires a real price, never a fake zero.
        ("invInventory", "GetPriceForItems") => Err(notify_error(crate::messages::inventory::ITEM_PRICES_UNSUPPORTED)),

        // ccSvc.py:_QAPickStarterSystem sorts the IDs and uses normal allocation when empty.
        ("charUnboundMgr", "GetQAStarterSystemIDs") => Ok(Value::list([])),
        // petitionWindow.py:CategoryScreen iterates characterID and characterName before selection.
        ("charUnboundMgr", "GetCharacterInfo") => Ok(Value::list([])),
        // evemissions/client/qa_tools.py:_select_mission stops on an empty candidate list.
        ("agentMgr", "GetPlausibleMissionIDs") => Ok(Value::list([])),

        // ledgerSvc.py:LedgerSvc.GetStructuresForCorpData reads solarSystemID, itemID and itemName.
        ("corpMiningLedger", "GetObserversWithMiningEvents") => Ok(Value::list([])),
        // ledgerSvc.py:LedgerSvc.GetCorpData forwards rows to ledgerEntry.py:GetLedgerEntriesCorp;
        // ledgerEntry.py:_GetDataForEntry reads eventDate, corporationID, characterID, typeID and quantity.
        ("corpMiningLedger", "GetObserverLedger") => Ok(Value::list([])),

        // mobilecollect/client/mobilecollect_svc.py:MobilecollectService.OnUnisdkBindMobileSucceed
        // ignores the reply and closes the open window.
        ("mobileCollectMgr", "MobileBindSucceed") => Ok(Value::None),

        // NetEase account/payment gateway; endpoints are never contacted here.
        ("kiringMgr", "GetKiringConfiguration") => Ok(Value::dict([
            ("mode", s("release")),
            ("client_id", s("infinity-emu")),
            (
                "endpoints",
                Value::dict([("mpay", s("http://127.0.0.1/")), ("billing", s("http://127.0.0.1/")), ("redirect_uri", s("http://127.0.0.1/"))]),
            ),
            ("channels", Value::dict([("login", s("emu")), ("pay", s("emu")), ("app", s("emu"))])),
        ])),
        ("kiringMgr", "IsMonochromeStyleEnabled") => Ok(Value::Bool(false)),
        // kiring.common.const.ANTI_ADDICTION_PASS_CODE
        ("kiringMgr", "GetFaceRecognitionCode") => Ok(Value::tuple([Value::Int(ANTI_ADDICTION_PASS_CODE), s("")])),
        ("kiringMgr", "GetAntiAddictionCode") => Ok(Value::Int(ANTI_ADDICTION_PASS_CODE)),
        ("kiringMgr", "IsZhStoreEnabled") => Ok(Value::Bool(false)),

        ("pingService", "Ping") => Ok(Value::None),
        // {solarSystemID: slotKey} of alternative load systems; empty = load where the character is.
        ("populationCap", "GetCharacterLoadSlot") => Ok(Value::Dict(vec![])),

        // {raceID: {solarSystemID: population}}; empty = no starter system is capped.
        ("populationCap", "GetCharacterCreationSystemPopulations") => Ok(Value::Dict(vec![])),

        // Login reward campaigns: none running.
        ("loginCampaignManager", "get_client_campaign_state") => Ok(Value::None),
        // loginCampaignService.py:claim_reward returns early on None; no reward was claimed.
        ("loginCampaignManager", "claim_reward") => Ok(Value::None),

        _ => return None,
    })
}

/// Fixed replies at the calendar/bookmark position in the dispatch chain.
pub(super) fn social_early(call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("notificationMgr", "LogNotificationInteraction") => Ok(Value::None),
        ("calendarMgr", "CreateAllianceEvent" | "EditAllianceEvent") => Err(notify_error(crate::messages::ALLIANCE_EVENTS_UNSUPPORTED)),
        _ => return None,
    })
}

/// Fixed replies at the character position in the dispatch chain.
pub(super) fn social(call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match (call.service, call.method) {
        ("charMgr", "GetPaperdollState") => Value::Int(PAPERDOLL_NO_RECUSTOMIZATION),
        ("charMgr", "SetActivityStatus") => Value::None,
        ("charMgr", "GetCharacterDescription") => Value::unicode(""),
        ("structureAssetSafety", "GetItemsInSafetyForCharacter" | "GetItemsInSafetyForCorp") => Value::list([]),
        ("bannedwords", "check_words_allowed") => Value::None,
        ("corpRegistry", "GetAggressionSettings") | ("corpStationMgr", "DoStandingCheckForStationService") | ("shipAdaptionMgr", "fetch_user_data") => Value::None,
        ("RWManager", "solarsystem_contains_rw_instances") => Value::Bool(false),
        ("dynamicBountyMgr", "GetOutputForClientSolarSystem") => Value::tuple([Value::None, Value::Bool(false)]),
        _ => return None,
    }))
}

pub(super) fn fleets(call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match (call.service, call.method) {
        ("fleetObjectHandler", "GetActiveBridgeForShip") => Value::None,
        _ => return None,
    }))
}

#[cfg(test)]
mod tests;
