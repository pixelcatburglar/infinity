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
//! Player structures are unsupported; their action entry points are refused.
//! `structureServices.py`, `structureGuests.py` and `modularHangarBehaviours.py`
//! receive empty structure reads and a default hangar display, not structure state.
//! Single-track login campaigns are closed; seasonal login campaigns remain separate.
//! `loginrewards/client/loginCampaignService.py` receives no campaign or claim.
//! Inventory configuration and item-price writes/reads are explicitly refused;
//! crystal damage is handled by the inventory feature module.
//! `facWarSvc.py`, `facwarCommon.py` and `shipcasterSvc.py` consume closed
//! warfare statistics, warzone points and projector lists; no warfare or projector
//! state is modelled.
//! `dynamicResourceSvc.py` and the ESS Agency views receive closed ESS data;
//! banks, links, keys, payouts and their history are not modelled.
//! Fighter movement, tube, recall, abandonment and ability actions, warp vectors
//! and positional superweapon activations are refused; their gameplay is not modelled.
//! The fighter snapshot stays empty, so fighter rearm bonus queries are not reached.
//! `lpStoreSvc.py` receives no nearest RW stations; RW gameplay is not modelled.
//! Moon extraction, structure compression and automatic moon drilling are
//! unavailable; closed reads return empty shapes and actions are refused.
//! The automatic miner window closes on a details refusal without starting updates.
//! `eve/client/script/ui/inflight/moonmining.py` receives an empty tower list;
//! tower settings, permissions, moon and fuel factor are refused, not modelled.
//! World-task and custom Agency catalogues are empty; task actions and content
//! travel are unavailable. Tale consumers receive no active event tales.
//! Operations progress is empty and no category is active; unmodelled tutorial
//! queries and actions are refused rather than reporting fabricated success.
//! `operationDestinationController.py` and `taskObjectiveData.py` receive no
//! tutorial scene destination or site; `scenarioMgr.py` cannot open the editor.
//! Tutorial instances and the level editor are not modelled.
//! Void-space, random filament and starter shipcaster travel are unavailable;
//! readiness and interference display replies carry no travel authorization.
//! `void_space_service.py` and `solarsystemInterferenceSvc.py` consume these replies.
//! `surveySvc.py` and `pvpFilamentSvc.py` receive closed survey and event
//! states; rewards, events and queues are not modelled or persisted.
//! Event details are unreachable without event data and remain unhandled.
//! `eveguard_service.py`, `unisdk_svc.py`, `apigateway.py` and character
//! selection receive closed platform compatibility replies. External guard,
//! authentication, payment and RMT services are not implemented.
//! Face verification (`faceVerify/client/faceverify_svc.py`) and webview
//! authentication (`minigame/client/ceve_mahjong.py`) are refused without
//! issuing identity state or proofs; their external platform services are unavailable.
//! `reportSvc.py`, `petitionWindow.py`, `petition.py`, `help.py`, and
//! `notificationCenter.py` receive closed support compatibility replies without
//! data sources or state. Reports and ticket actions are refused; no support
//! backend or external links exist.
//! Account actions, reports, votes and QA mutations are refused without state;
//! account identity and character energy reads remain unhandled.
//! Seasonal progress and battlepass content are absent; no seasonal callbacks
//! or milestone timers are sent. Delayed reward groups are empty, while
//! seasonal data reload and ticket redemption are refused.
//! The Serenity reply preserves the legacy display and is not a region setting.
//! NES introduction remains inactive; raffle seeding and ticket injection are
//! refused. Paid offers, cloud computing and their external backends are not modelled.
//! The monetization user request is a compatibility acknowledgement, not user data.
//! The tournament referee catalogue is empty: no tournament or system can be
//! selected, and competitive tournament gameplay and ban notifications are not modelled.

use super::{keyval, notify_error, rowset, Call, CallResult};
use super::universe::{WARZONES, DYNAMIC_RESOURCE_MAX_OUTPUT};
use infinity_core::consts::{GROUP_ALLIANCE, GROUP_CHARACTER, GROUP_CORPORATION};
use marshal::Value;
/// storylines/common/nes_intro_constants.py: NesIntroState.UNSET.
const NES_INTRO_UNSET: i64 = 0;
/// Client paperdoll.State.no_recustomization.
const PAPERDOLL_NO_RECUSTOMIZATION: i64 = 0;
/// dynamicresources/common/ess/const.py: ESS_AUTOPAY_STATE_LONG.
const ESS_AUTOPAY_STATE_LONG: i64 = 0;
/// dynamicresources/common/ess/const.py: ESS_RESERVE_WINDOW_STATE_INACTIVE.
const ESS_RESERVE_WINDOW_STATE_INACTIVE: i64 = 0;
/// Closed-warzone server policy: facwarCommon.GetDonationTax requires a nonzero denominator.
const CLOSED_WARZONE_MAX_POINTS: i64 = 1;
/// Closed-display server policy: the current interference level is zero.
const CLOSED_INTERFERENCE_LEVEL: f64 = 0.0;
/// Closed-display server policy: no decay tick has occurred.
const CLOSED_INTERFERENCE_LAST_DECAY_TICK_TIMESTAMP: i64 = 0;
/// Closed-display server policy: use a positive decrease per decay tick;
/// the constructor in `solarsystemInterferenceState.py` checks that it is > 0.
const CLOSED_INTERFERENCE_DECREASE_PER_DECAY_TICK: f64 = 1.0;
/// Closed-display server policy: use a positive decay tick period;
/// the constructor in `solarsystemInterferenceState.py` checks that it is > 0.
const CLOSED_INTERFERENCE_DECAY_TICK_PERIOD: i64 = 1;
/// Closed-display server policy: the maximum interference level is one.
const CLOSED_INTERFERENCE_MAX_LEVEL: f64 = 1.0;
/// Closed-display server policy: the quiescent interference level is zero.
const CLOSED_INTERFERENCE_QUIESCENT_LEVEL: f64 = 0.0;

fn empty_warfare_stats() -> Value {
    Value::Dict([GROUP_CHARACTER, GROUP_CORPORATION, GROUP_ALLIANCE].into_iter().map(|group| {
        (Value::Int(group), Value::dict(["killsY", "killsLW", "killsTotal", "vpY", "vpLW", "vpTotal"].map(|key| (key, Value::Int(0)))))
    }).collect())
}

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
        ("nes_intro", "get_nes_intro_state") => Value::Int(NES_INTRO_UNSET),
        ("taleMgr", "get_active_tales_by_template" | "GetGlobalWorldEventTales") => Value::list([]),
        ("map", "GetMyExtraMapInfo") => Value::None,
        ("map", "GetMyExtraMapInfoAgents") => rowset(&["fromID", "rank"], vec![]),
        ("map", "GetDeadspaceComplexMap") | ("map", "GetDeadspaceAgentsMap") => Value::None,
        ("map", "GetBeaconCount") => Value::Dict(vec![]),
        ("map", "GetIndustryJobsOverLast") => rowset(&history_header, vec![]),
        ("map", "GetInsurgencySystem") => Value::list([]),
        // facWarSvc reads the upgrade map and faction points; the denominator must be nonzero.
        ("map", "GetFacWarZoneInfo") => keyval([("systemUpgradeLevel", Value::Dict(vec![])), ("factionPoints", Value::Int(0)), ("maxWarZonePoints", Value::Int(CLOSED_WARZONE_MAX_POINTS))]),
        // facWarSvc reads killsY, killsLW, killsTotal, vpY, vpLW and vpTotal for each owner group.
        ("facWarMgr", "GetStats_TopAndAllKillsAndVPs") => Value::tuple([empty_warfare_stats(), empty_warfare_stats()]),
        ("facWarMgr", "GetStats_Character" | "GetStats_Corp" | "GetStats_Alliance") => Value::Dict(vec![]),
        // shipcasterSvc iterates these lists; closed projectors have no pads or factions.
        ("shipCasterLandingPadMgr", "GetFactionLandingPads" | "GetAllLandingPads")
        | ("shipCasterLauncherMgr", "GetFactionsWithShipcaster") => Value::list([]),
        ("map", "GetAllRoamingWeatherSystems") => Value::Dict(vec![]),
        ("map", "GetTriglavianMinorVictorySystems") | ("map", "GetEdencomMinorVictorySystems") | ("map", "GetEdencomFortressSystems") => {
            Value::list([])
        }
        ("dynamicResourceCacheMgr", "GetDBSMapData") => Value::Dict(vec![]),
        ("dynamicResourceCacheMgr", "GetESSAgencyData") => Value::Dict(vec![]),
        ("dynamicResourceCacheMgr", "GetESSReserveBankKeys") => Value::list([]),
        // dynamicresources/common/ess/const.py: LONG and INACTIVE are both zero.
        ("dynamicResourceCacheMgr", "GetESSSystemDetails") => Value::dict([
            ("autopaymentWindowState", Value::Int(ESS_AUTOPAY_STATE_LONG)),
            ("activeLinks", Value::Int(0)),
            ("totalPulses", Value::Int(0)),
            ("reserveTimeWindowState", Value::Int(ESS_RESERVE_WINDOW_STATE_INACTIVE)),
            ("reserveTimeChosen", Value::Int(0)),
        ]),
        // Bounty output range and its equilibrium (mapFilterDynamicBounty reads all three).
        ("dynamicResourceCacheMgr", "GetDynamicResourceSettings") => Value::dict([
            ("minOutput", Value::Float(0.0)),
            ("maxOutput", Value::Float(DYNAMIC_RESOURCE_MAX_OUTPUT)),
            ("equilibriumValue", Value::Float(1.0)),
            // essBountiesOutputTooltip.CreateTooltip: positive display duration.
            ("timeIntervalSeconds", Value::Int(1)),
            // essGameplayTooltip.CreateTooltip: no bounty allocation to a closed bank.
            ("bountiesCut", Value::Float(0.0)),
            // essMainBankTooltip.CreateTooltip and controller.GetProportionOfTimeLeftForMainBankHack:
            // positive intervals avoid zero denominators; a closed bank pays nothing.
            ("autopaymentIntervalSeconds", Value::Int(1)),
            ("bufferTimeSeconds", Value::Int(1)),
            ("mainBankPayRatio", Value::Float(0.0)),
            // dynamicresources/client/ess/settings.py:EssSettings._update_from_cache_data:
            // positive timedelta for cache consumers.
            ("reservePulseIntervalSeconds", Value::Int(1)),
            // infoPanels/essUI/controller.py:CalculateReserveBankPayouts and
            // dynamicresources/common/ess/formulas.py:GetPayout: positive divisor and
            // exponents, with zero payout and padding for the closed bank.
            ("reserveWindowAccessFactor", Value::Float(1.0)),
            ("reservePayoutExponentLead", Value::Float(1.0)),
            ("reservePayoutExponentTail", Value::Float(1.0)),
            ("reservePayoutPaddingValue", Value::Float(0.0)),
            ("reserveBankMaxPayout", Value::Float(0.0)),
            // essSystemInfoContainer.ConstructPayoutStateContainer: empty threshold fills.
            ("earlyPayoutThresholdFactor", Value::Float(0.0)),
            ("latePayoutThresholdFactor", Value::Float(0.0)),
            // essReserveBankTooltip.CreateTooltip and essSystemInfoContainer._UpdateContentPiece:
            // positive display factor for a closed reserve bank.
            ("reserveBankTimeAccessFactor", Value::Float(1.0)),
        ]),
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
        ("corpRegistry", "GetStructureReinforceDefault") => Value::None,
        ("corpRegistry", "DoesMyCorpAcceptStructures") | ("corpRegistry", "DoesCorpRestrictCorpMails") => Value::Bool(false),
        ("allianceRegistry", "GetDaysInAlliance") => Value::Int(0),
        ("allianceRegistry", "GetAllianceMembersOlderThan") => Value::list([]),

        // ---- wallet ----
        ("rewardMgr", "GetRewardLPLogs") => Value::list([]),
        ("rewardMgr", "GetDelayedRewardsByGroupIDs") => Value::Dict(vec![]),
        ("account", "GetJournal") | ("account", "GetJournalForAccounts") => Value::list([]),
        // Wallet owner picker has no modelled account-owner search.
        ("lookupSvc", "LookupNoneNPCAccountOwners") => Value::list([]),
        // Legacy location search has no modelled group lookup.
        ("lookupSvc", "LookupKnownLocationsByGroup") => Value::list([]),
        // Developer location search has no modelled group lookup.
        ("lookupSvc", "LookupLocationsByGroup") => Value::list([]),
        // Developer character search has no modelled generic lookup.
        ("lookupSvc", "LookupCharacters") => Value::list([]),
        // Developer corporation search has no modelled generic lookup.
        ("lookupSvc", "LookupCorporations") => Value::list([]),
        // Developer faction search has no modelled generic lookup.
        ("lookupSvc", "LookupFactions") => Value::list([]),

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
        ("essMgr", "GetMainBankTheftsForClientSolarSystem" | "GetReserveBankTheftsForClientSolarSystem") => Value::list([]),
        ("essMgr", "IsClientLinkedToReserveBank") => Value::Bool(false),
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
        // Compatibility acknowledgements; no monetization user data or notification is supplied.
        ("monetizationMgr", "CallUserRequested") | ("exchangeShopMgr", "CallExchangeShopOpened") => Value::None,
        ("milestoneMgr", "ProcessCharacterLogon") => Value::None,
        // structureServices.CharacterGetServices/IsServiceAvailableForCharacter and
        // structureGuests.GetGuests cache and inspect these mappings.
        ("structureSettings", "CharacterGetServices") | ("structureGuests", "GetGuests") => empty_dict(),
        // modularHangarBehaviours.UpdateStructureState reads three damage slots,
        // converts booleans to floats and uses optional timer fields.
        ("structureHangarViewMgr", "GetMyHangarViewState") => keyval([
            ("upkeepState", Value::None),
            ("operatingState", Value::None),
            ("damageState", Value::tuple([Value::Float(1.0), Value::Float(1.0), Value::Float(1.0)])),
            ("timerProgress", Value::None),
            ("timerIsProgressing", Value::Bool(false)),
            ("timerIsPaused", Value::Bool(false)),
            ("timerEndAt", Value::None),
        ]),
        ("activityMgr", "GetActivityBadgeStatuses") => Value::list([]),
        ("activityMgr", "ActivitySeen") => Value::None,
        ("operationsManager", "can_character_play_the_tutorial") => Value::Bool(false),
        ("insurgencySolarsystem", "GetAllVisibleCampaigns") => Value::list([]),
        ("battlepassSeasonMgr", "CallSeasonDataRequested") => Value::None,
        ("clientStatLogger", _) => Value::None,
        // [ownerID, itemID, operationID, stationTypeID]
        _ => return None,
    }))
}

/// Fixed answers that originally preceded feature dispatch.
pub(super) fn root(call: &Call<'_>) -> Option<CallResult> {
    let s = Value::str;
    Some(match (call.service, call.method) {
        ("tourneyMgr", "GetTourneySetup") => Ok(Value::Tuple(vec![
            Value::Dict(vec![]), Value::Dict(vec![]), Value::List(vec![]),
        ])),
        ("raffleMgr", "QA_SeedRaffles") => Err(notify_error(infinity_core::messages::raffles::SEEDING_UNAVAILABLE)),
        ("gachaMgr", "InjectTickets") => Err(notify_error(infinity_core::messages::gacha::TICKET_INJECTION_UNAVAILABLE)),
        ("seasonManager", "reload_fsd_data") => Err(notify_error(infinity_core::messages::season::ACTION_UNAVAILABLE)),
        ("bluebirdIndustryMgr", "RedeemTicket") => Err(notify_error(infinity_core::messages::bluebird::TICKET_REDEMPTION_UNAVAILABLE)),
        ("ingamereport", "report_inappropriate_language" | "report_offensive_mail"
            | "report_macro_use" | "report_questionable_transaction"
            | "report_inappropriate_character_name" | "report_exploit_abuse"
            | "report_gambling" | "report_other" | "report_protection_of_minors") =>
            Err(notify_error(infinity_core::messages::support::REPORT_UNAVAILABLE)),
        ("petitioner", "MayPetition" | "CreatePetition" | "UpdatePetitionRating"
            | "AddPetitionRating" | "PetitioneeChat" | "PetitionerChat"
            | "DeletePetition" | "CancelPetition" | "ClosePetition" | "UnClaimPetition"
            | "EscalatePetition" | "ClaimPetition" | "GetZendeskJwtLink") =>
            Err(notify_error(infinity_core::messages::support::PETITION_UNAVAILABLE)),
        ("petitioner", "GetCategoryHierarchicalInfo") => Ok(Value::tuple([
            Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![]),
        ])),
        ("petitioner", "GetMyPetitionsEx" | "GetPetitionMessages") => Ok(Value::list([])),
        ("petitioner", "MarkAsRead") => Ok(Value::None),
        ("petitioner", "HasOpenTickets") => Ok(Value::Bool(false)),
        ("petitioner", "IsSerenity") => Ok(Value::Bool(true)),
        // surveySvc.py treats the active ID as optional and ignores check and claim replies.
        ("survey", "ClaimSurveyRewards" | "PerformSurveyChecks" | "GetActiveSurveyID") => Ok(Value::None),
        // pvpFilamentSvc.py builds event data from these dictionaries.
        ("pvpFilamentMgr", "GetAllEvents" | "GetActiveEvents") => Ok(Value::Dict(vec![])),
        ("pvpFilamentMgr", "GetMostRecentEvent" | "GetNextEventDate") => Ok(Value::None),
        ("pvpFilamentMgr", "JoinPVPQueue" | "LeavePVPQueue") => Err(notify_error(infinity_core::messages::pvp_filaments::UNAVAILABLE)),
        ("voidSpaceMgr", "ClientIsReady") => Ok(Value::None),
        ("voidSpaceMgr", "VoidSpacePlayerJump") => Err(notify_error(infinity_core::messages::void_space::UNAVAILABLE)),
        ("randomJumpMgr", "ActivateRandomJumpFilament") => Err(notify_error(infinity_core::messages::random_jump::UNAVAILABLE)),
        ("starterShipcasterTravelMgr", "CanCharacterJump") => Err(notify_error(infinity_core::messages::shipcaster::UNAVAILABLE)),
        ("solarsystemInterferenceMgr", "GetAllInterferenceBands") => Ok(Value::Dict(vec![])),
        ("solarsystemInterferenceMgr", "GetLocalInterferenceState") => Ok(Value::reduce(
            "solarsysteminterference.solarsystemInterferenceState.SolarsystemInterferenceState",
            vec![
                Value::Float(CLOSED_INTERFERENCE_LEVEL),
                Value::Int(CLOSED_INTERFERENCE_LAST_DECAY_TICK_TIMESTAMP),
                Value::Float(CLOSED_INTERFERENCE_DECREASE_PER_DECAY_TICK),
                Value::Int(CLOSED_INTERFERENCE_DECAY_TICK_PERIOD),
                Value::Float(CLOSED_INTERFERENCE_MAX_LEVEL),
                Value::Float(CLOSED_INTERFERENCE_QUIESCENT_LEVEL),
            ],
            None,
        )),
        ("custom_agency_provider", "get_content_data") => Ok(Value::list([])),
        ("worldTaskContentMgr", "GetDungeonShipRestrictions")
        | ("custom_agency_provider", "warp_to_content") => Err(notify_error(infinity_core::messages::world_tasks::UNAVAILABLE)),
        ("keeper", "GetOperationSpawnpointSolarSystem") => Ok(Value::None),
        ("keeper", "IsOperationSite") => Ok(Value::Bool(false)),
        ("keeper", "GetLevelEditor") => Err(notify_error(infinity_core::messages::npe::LEVEL_EDITOR_UNAVAILABLE)),
        // structureSettings.py reads these as permission booleans.
        ("structureSettings", "CharacterHasSetting" | "CharacterHasService") => Ok(Value::Bool(false)),
        // structureControl.py and jump bridge readers treat None as no pilot or destination.
        ("structureControl", "GetStructurePilot")
        | ("structureJumpBridgeMgr", "GetJbStructureDestination" | "GetLinkedStructure") => Ok(Value::None),
        // assetSafetyDeliverWindow.py unpacks destinations and the selected destination.
        ("structureAssetSafety", "GetStructuresICanDeliverTo") => Ok(Value::tuple([Value::list([]), Value::None])),
        // assetSafetySvc.py updates its wrap-name dictionary from this result.
        ("structureAssetSafety", "GetWrapNames") => Ok(Value::Dict(vec![])),
        // allProfilesController.py iterates profiles; no default profile exists.
        ("structureProfiles", "GetProfiles") => Ok(Value::list([])),
        // Structure action clients must see refusal, not an apparently successful mutation.
        ("structureSettings", "CharacterGetService" | "CharacterCheckService")
        | ("structureDeployment", "RenameStructure" | "Unanchor" | "CancelUnanchor" | "Anchor")
        | ("structureDeliveries", "Deliver")
        | ("structureControl", "TakeControl" | "ReleaseControl")
        | ("structureDocking", "Undock" | "Dock")
        | ("structureVulnerability", "UpdateReinforceTimesFromClient")
        | ("structureCargoDelivery", "DropOffItems")
        | ("structureAssetSafety", "MoveEjectTimeGM" | "MoveSafetyWrapToStructure" | "MovePersonalAssetsToSafety" | "MoveCorpAssetsToSafety")
        | ("structureCynoBeaconMgr", "CmdBridgeToStructureBeacon" | "CmdGroupJumpToStructureBeacon")
        | ("structureProfiles", "CreateProfile") => Err(notify_error(infinity_core::messages::structures::UNAVAILABLE)),
        ("machoNet", "GetServiceInfo") => Ok(Value::Dict(vec![])),
        ("air_npe", "is_air_npe_enabled") => Ok(Value::Bool(false)),
        // Operations initialization consumes empty progress and no active category.
        ("operationsManager", "get_character_progress") => Ok(Value::Dict(vec![])),
        ("operationsManager", "get_active_category_id") => Ok(Value::None),
        ("operationsManager", "get_operations_completed_at_least_once" | "get_mission_avoidance_systems") => Ok(Value::list([])),
        ("operationsManager", "skip_task" | "activate_operation_in_solar_system" | "is_category_complete") => Ok(Value::Bool(false)),
        ("operationsManager", "get_tutorial_state" | "is_main_tutorial_finished" | "has_skipped_tutorial" | "start_site" | "complete_category_for_character" | "block_task" | "unblock_task" | "get_category_state" | "process_client_event") => Err(notify_error(infinity_core::messages::npe::OPERATIONS_UNAVAILABLE)),
        // cynoJammerIcon.py treats None as no interference.
        ("sovDashboard", "GetCynoJammerState") => Ok(Value::None),

        ("fighterMgr", "ExecuteMovementCommandOnFighters" | "LoadFightersToTube" | "UnloadTubeToFighterBay" | "LaunchFightersFromTubes" | "RecallFightersToTubes" | "CmdAbandonFighter" | "CmdScoopAbandonedFighterFromSpace" | "CmdActivateAbilitySlots" | "CmdDeactivateAbilitySlots") => Err(notify_error(infinity_core::messages::fighters::UNAVAILABLE)),
        ("warpVectorMgr", "UseVector") => Err(notify_error(infinity_core::messages::warp_vector::UNAVAILABLE)),
        ("superWeaponMgr", "ActivateSinglePointTargetedModule" | "ActivateSlashModule") => Err(notify_error(infinity_core::messages::super_weapon::UNAVAILABLE)),
        ("essMgr", "AttemptLinkToMainBank" | "AttemptLinkToReserveBank" | "RequestMainBankUnlink" | "RequestReserveBankUnlink" | "RequestUnlockReserveBank") => Err(notify_error(infinity_core::messages::ess::UNAVAILABLE)),
        // menuFunctions.py:DecryptItem cannot open media without a supported source; refuse without consuming the item.
        ("encodedItems", "DecodeItem") => Err(notify_error(infinity_core::messages::encoded_items::DECODE_UNAVAILABLE)),
        // invControllers.py:BaseInvContainer.__AddItem reads this root mask.
        ("invInventory", "ALSCConfigGet") => Ok(Value::Int(0)),
        // ConfigureALSC/menuFunctions.py writes config; persistence is unsupported.
        ("invInventory", "ALSCConfigSet") => Err(notify_error(infinity_core::messages::inventory::ALSC_CONFIG_UNSUPPORTED)),
        // invCache.py:AcceptPossibleRemovalTax requires a real price, never a fake zero.
        ("invInventory", "GetPriceForItems") => Err(notify_error(infinity_core::messages::inventory::ITEM_PRICES_UNSUPPORTED)),

        // ccSvc.py:_QAPickStarterSystem sorts the IDs and uses normal allocation when empty.
        ("charUnboundMgr", "GetQAStarterSystemIDs") => Ok(Value::list([])),
        // petitionWindow.py:CategoryScreen iterates characterID and characterName before selection.
        ("charUnboundMgr", "GetCharacterInfo") => Ok(Value::list([])),
        // evemissions/client/qa_tools.py:_select_mission stops on an empty candidate list.
        ("agentMgr", "GetPlausibleMissionIDs") => Ok(Value::list([])),

        // Multi-character training is not modelled; activation cannot consume anything.
        ("userSvc", "GetUpdatedMultiCharacterTraining") => Ok(Value::list([])),
        ("userSvc", "ActivateMultiTraining" | "ActivateSoulboundMultiTraining") => Err(notify_error(infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED)),
        // skillQueueSvc.py invalidates its cache before fetching training slots.
        ("userSvc", "InvalidateMultiCharacterTrainingCache") => Ok(Value::None),
        ("userSvc", "ActivateRenameCard" | "ActivateCharacterReSculpt" | "ReverseRedeem"
            | "ReportISKSpammer" | "CastUpvote" | "CastDownvote" | "ReportBot"
            | "CreateRedeemTokenQA")
        | ("communityTrackMgr", "change_qa_mode") => Err(notify_error(infinity_core::messages::account::ACTION_UNAVAILABLE)),

        // ledgerSvc.py:LedgerSvc.GetStructuresForCorpData reads solarSystemID, itemID and itemName.
        ("corpMiningLedger", "GetObserversWithMiningEvents") => Ok(Value::list([])),
        // ledgerSvc.py:LedgerSvc.GetCorpData forwards rows to ledgerEntry.py:GetLedgerEntriesCorp;
        // ledgerEntry.py:_GetDataForEntry reads eventDate, corporationID, characterID, typeID and quantity.
        ("corpMiningLedger", "GetObserverLedger") => Ok(Value::list([])),

        // schedulingSvc.py unpacks the extraction/event pair and iterates corp extractions.
        ("moonExtractions", "GetExtractionAndEventIDFromStructureID") => Ok(Value::tuple([Value::None, Value::None])),
        ("moonExtractions", "GetExtractionsForCorp") => Ok(Value::list([])),
        ("moonExtractions", "GetMoonResourcesForStructure") => Ok(Value::Dict(vec![])),
        ("moonExtractions", "StartNewExtraction" | "FractureChunkWithStructure" | "CancelExtraction") => Err(notify_error(infinity_core::messages::mining::MOON_EXTRACTION_UNAVAILABLE)),
        ("structureCompressionMgr", "DecompressGasInStructure" | "GetMyGasDecompressionEfficiency" | "CompressItemInStructure") => Err(notify_error(infinity_core::messages::mining::STRUCTURE_COMPRESSION_UNAVAILABLE)),
        ("autoMoonMining", "RequestMiningDetails") => Err(notify_error(infinity_core::messages::mining::AUTO_MOON_MINING_UNAVAILABLE)),
        // OutputMaterialsSection iterates the per-cycle material dictionary.
        ("autoMoonMining", "RequestMiningCycleOutput") => Ok(Value::Dict(vec![])),

        // ShowOtherTowers iterates the list; tower-specific views require real state.
        ("posMgr", "GetControlTowers") => Ok(Value::list([])),
        ("posMgr", "GetTowerSentrySettings" | "GetTowerNotificationSettings" | "GetStarbasePermissions" | "GetMoonForTower" | "GetConsumptionFactor") => Err(notify_error(infinity_core::messages::structures::STARBASE_UNAVAILABLE)),

        // mobilecollect/client/mobilecollect_svc.py:MobilecollectService.OnUnisdkBindMobileSucceed
        // ignores the reply and closes the open window.
        ("mobileCollectMgr", "MobileBindSucceed") => Ok(Value::None),

        // Guard handshake must refuse rather than return None and trigger an external fallback.
        ("kiringMgr", "GetEveGuardServerAddress") => Err(notify_error(infinity_core::messages::platform::GUARD_UNAVAILABLE)),
        ("kiringMgr", "ShouldKickCheater") => Ok(Value::Bool(false)),
        ("eveguard_report", "verify_cloud_game_disable_key") => Ok(Value::Bool(false)),
        ("eveguard_report", "heartbeat_started" | "heartbeat_stopped") => Ok(Value::None),
        // characterSelection.py only blocks selection on a false reply.
        ("multiLoginBlocker", "Login") => Ok(Value::Bool(true)),
        ("kiringMgr", "RMTQuerySellOrdersByCharacterID") => Ok(Value::list([])),
        ("kiringMgr", "GetUnisdkPaymentConfig") => Ok(Value::Dict(vec![])),
        ("kiringMgr", "RMTSellIsk" | "RMTSignup" | "RMTCancelSignup" | "RMTSellCharacter"
            | "RMTRetrySaleOrder" | "RMTRetrySaleOrderWhenLogin" | "RMTTakeBackItem"
            | "RMTTakeBackItemWhenLogin" | "RMTTakeAwayItem") => Err(notify_error(infinity_core::messages::platform::RMT_UNAVAILABLE)),
        ("kiringMgr", "PerformKiringServerSideAuthenticationFromCode" | "GetSAuthAid") => Err(notify_error(infinity_core::messages::platform::AUTH_UNAVAILABLE)),
        // Face verification and webview bridges cannot issue external authentication.
        ("faceverifyMgr", "FaceVerifySucceed")
        | ("webviewAuthService", "GetWebviewForwardToken") => Err(notify_error(infinity_core::messages::platform::AUTH_UNAVAILABLE)),
        ("kiringMgr", "PlaceKiringOrder" | "ActivateRedeemingCode") => Err(notify_error(infinity_core::messages::platform::PAYMENT_UNAVAILABLE)),

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
        ("calendarMgr", "CreateAllianceEvent" | "EditAllianceEvent") => Err(notify_error(infinity_core::messages::ALLIANCE_EVENTS_UNSUPPORTED)),
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
        ("corpRegistry", "GetAggressionSettings") | ("corpStationMgr", "DoStandingCheckForStationService") => Value::None,
        ("RWManager", "solarsystem_contains_rw_instances") => Value::Bool(false),
        // lpStoreSvc.GetNearestDockableWithLPStoreForCorp looks up the corporation in this mapping.
        ("RWManager", "get_closest_rw_stations") => Value::Dict(vec![]),
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
