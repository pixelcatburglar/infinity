# Remote service coverage

This document lists client remote-service calls the server answers specially or intentionally leaves unhandled, grouped by feature: handled, fixed, retail, deferred-no-handler, or out of scope. Rows are added as calls are dealt with. A call not listed is answered by its feature module or logged as unhandled.

## Status definitions

- **handled**: A real implementation handles the method.
- **fixed**: A constant compatibility answer is provided for an unmodelled feature; this does not mean the feature is fully supported.
- **retail**: A static retail capture provides the answer.
- **deferred-no-handler**: The feature is deferred, not out of scope; a documented outer client gate closes the normal path, so no handler is provided and forced calls remain unhandled.
- **out-of-scope**: The feature is unavailable.

## Competitive tournaments

Competitive tournament gameplay is deferred. The empty catalogue prevents a
tournament selection, so match and detail queries are not reached normally.
Ship-list queries require a ban notification, which the server does not send.
Forced calls to those methods remain unhandled.

| service | method | status | handler | answer |
|---|---|---|---|---|
| tourneyMgr | GetTourneySetup | fixed | `crates/server/src/services/fixed.rs` | Tuple of empty tournament and system dictionaries and an empty active-system list |
| tourneyMgr | GetPotentialMatches | deferred-no-handler | no handler | Unhandled; no tournament selection |
| tourneyMgr | GetAllCurrentSeriesMatchDetails | deferred-no-handler | no handler | Unhandled; no tournament selection |
| tourneyMgr | QueryTournamentDetails | deferred-no-handler | no handler | Unhandled; no tournament selection |
| tourneyMgr | QueryShipList | deferred-no-handler | no handler | Unhandled; no ban notification |

## Brawl compatibility

Full brawl gameplay remains deferred: no matches or instances are created.
Actions receive a CustomNotify refusal. Recovery returns None and sends only the
requester an OUTSIDE status, not fabricated character data; this legacy status
notification does not reset the gateway controller.

| service | method | status | handler | answer |
|---|---|---|---|---|
| brawlMgr | CancelMatch | fixed | `crates/server/src/services/brawl/brawl_mgr.rs` | CustomNotify refusal |
| brawlMgr | LaunchMatch | fixed | `crates/server/src/services/brawl/brawl_mgr.rs` | CustomNotify refusal |
| brawlMgr | LeaveFight | fixed | `crates/server/src/services/brawl/brawl_mgr.rs` | CustomNotify refusal |
| brawlMgr | ChooseSpawnDungeon | fixed | `crates/server/src/services/brawl/brawl_mgr.rs` | CustomNotify refusal |
| brawlMgr | launch_match | fixed | `crates/server/src/services/brawl/brawl_mgr.rs` | CustomNotify refusal |
| brawlMgr | StartNewPlayerFight | fixed | `crates/server/src/services/brawl/brawl_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | EnableSkillUpgrade | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | Step3 | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | ChangeZoneStatusToPreset | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | ActiveGate | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | GenerateCloud | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | CollectCloud | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | ReleaseCloud | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | SpawnFakePlayer | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | UnlimitedEntropy | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | ActiveEntropyModule | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | Leave | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | Step2 | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlNewPlayerMgr | Step1 | fixed | `crates/server/src/services/brawl/brawl_new_player_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | FetchCharacterData | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | None and requester-only OUTSIDE notification |
| brawlGameplayMgr | DropCloud | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | AutoEquip | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | ActiveEntropyModule | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | UpgradeEntropyModule | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | UnlockEntropyModule | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | ReleaseCloud | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | CollectCloud | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |
| brawlGameplayMgr | TakeKillingMedal | fixed | `crates/server/src/services/brawl/brawl_gameplay_mgr.rs` | CustomNotify refusal |

## Support and reports

Support history, messages and categories are closed. No reports or tickets are
accepted and no support backend, redirect or JWT is provided. The Serenity
answer preserves the legacy petition display; it does not identify the server
region or enable external support.

| service | method | status | handler | answer |
|---|---|---|---|---|
| ingamereport | report_inappropriate_language | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_offensive_mail | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_macro_use | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_questionable_transaction | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_inappropriate_character_name | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_exploit_abuse | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_gambling | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_other | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| ingamereport | report_protection_of_minors | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | GetUserCatalogCountry | out-of-scope | no handler | unhandled |
| petitioner | MayPetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | GetCategoryProperties | out-of-scope | no handler | unhandled |
| petitioner | PropertyPopulationInfo | out-of-scope | no handler | unhandled |
| petitioner | GetClientPickerInfo | out-of-scope | no handler | unhandled |
| petitioner | CreatePetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | GetCategoryHierarchicalInfo | out-of-scope | `crates/server/src/services/fixed.rs` | Tuple of four empty dictionaries |
| petitioner | MarkAsRead | out-of-scope | `crates/server/src/services/fixed.rs` | None acknowledgement |
| petitioner | GetMyPetitionsEx | out-of-scope | `crates/server/src/services/fixed.rs` | Empty list |
| petitioner | GetClaimedPetitions | out-of-scope | no handler | unhandled |
| petitioner | UpdatePetitionRating | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | AddPetitionRating | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | GetPetitionQueue | out-of-scope | no handler | unhandled |
| petitioner | GetQueues | out-of-scope | no handler | unhandled |
| petitioner | GetCategories | out-of-scope | no handler | unhandled |
| petitioner | GetLog | out-of-scope | no handler | unhandled |
| petitioner | GetPetitionMessages | out-of-scope | `crates/server/src/services/fixed.rs` | Empty list |
| petitioner | PetitioneeChat | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | PetitionerChat | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | DeletePetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | CancelPetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | ClosePetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | UnClaimPetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | EscalatePetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | ClaimPetition | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| petitioner | IsSerenity | out-of-scope | `crates/server/src/services/fixed.rs` | Bool true |
| petitioner | HasOpenTickets | out-of-scope | `crates/server/src/services/fixed.rs` | Bool false |
| petitioner | GetZendeskJwtLink | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |

## External platform login compatibility

External guard, SDK, RMT, authentication and payments remain out of scope. These
exact compatibility answers use `crates/server/src/services/fixed.rs`; they
issue no external credential, URL, order or identity and initiate no external
effect. Browser authentication may already have opened an external URL before
the server refuses its RPC. The external multi-login blocker is disabled; this
does not change normal account live-session admission.

| service | method | status | handler | answer |
|---|---|---|---|---|
| kiringMgr | GetEveGuardServerAddress | out-of-scope | fixed.rs | CustomNotify refusal, preventing fallback URL |
| kiringMgr | ShouldKickCheater | out-of-scope | fixed.rs | Bool false |
| kiringMgr | RMTQuerySellOrdersByCharacterID | out-of-scope | fixed.rs | Empty list |
| kiringMgr | GetUnisdkPaymentConfig | out-of-scope | fixed.rs | Empty dict |
| kiringMgr | RMTSellIsk | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTSignup | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTCancelSignup | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTSellCharacter | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTRetrySaleOrder | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTRetrySaleOrderWhenLogin | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTTakeBackItem | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTTakeBackItemWhenLogin | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | RMTTakeAwayItem | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | PerformKiringServerSideAuthenticationFromCode | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | GetSAuthAid | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | PlaceKiringOrder | out-of-scope | fixed.rs | CustomNotify refusal |
| kiringMgr | ActivateRedeemingCode | out-of-scope | fixed.rs | CustomNotify refusal |
| eveguard_report | verify_cloud_game_disable_key | out-of-scope | fixed.rs | Bool false |
| eveguard_report | heartbeat_started | out-of-scope | fixed.rs | None |
| eveguard_report | heartbeat_stopped | out-of-scope | fixed.rs | None |
| multiLoginBlocker | Login | out-of-scope | fixed.rs | Bool true |

The following `kiringMgr` queries are out of scope and have no handler:
`CanSellCharacterToSpecific`, `GetZhStoreOfficialWebsiteUrl`,
`RMTQuerySellOrderList`, `RMTQueryBuyOrderList`, `RMTQueryHistoryOrderList`,
`RMTQuerySignupState`, `RMTQuerySellISKLimitations`,
`RMTQuerySellCharacterLimitations`, and `RMTQueryISKRecommendPrice`.

## Platform tools and external verification

The client calls EnvSDK methods only when `envsdk_enabled` is on; the server
never enables it, and turning it on locally is unsupported. Existing browser
flagged-site hash and list replies are unchanged. No external verification,
webview proof, moderation, QA operation, proxy or upload is implemented.

| service | method | status | handler | answer |
|---|---|---|---|---|
| faceverifyMgr | FaceVerifySucceed | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| webviewAuthService | GetWebviewForwardToken | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| browserLockdownSvc | GetDefaultHomePage | out-of-scope | no handler | unhandled |
| browserLockdownSvc | IsBrowserInLockdown | out-of-scope | no handler | unhandled |
| eveacsMgr | RequestHttpMethod | out-of-scope | no handler | unhandled |
| eveacsMgr | UploadReport | out-of-scope | no handler | unhandled |
| authentication | AmUnderage | out-of-scope | no handler | unhandled |
| authentication | AccruedTime | out-of-scope | no handler | unhandled |
| publicQaToolsServer | SlashCmd | out-of-scope | no handler | unhandled |
| publicQaToolsServer | CanGiveItemForMultifit | out-of-scope | no handler | unhandled |
| machoNet | SetGlobalConfigValue | out-of-scope | no handler | unhandled |
| machoNet | ReloadClientCodeHash | out-of-scope | no handler | unhandled |
| localizationServer | ReloadFSDPickle | out-of-scope | no handler | unhandled |
| marsRPCMgr | test1 | out-of-scope | no handler | unhandled |
| marsRPCMgr | test2 | out-of-scope | no handler | unhandled |
| marsRPCMgr | test7 | out-of-scope | no handler | unhandled |
| marsRPCMgr | test9 | out-of-scope | no handler | unhandled |
| marsRPCMgr | testSpawnNPC | out-of-scope | no handler | unhandled |
| serviceGatewayMgr | qa_is_hijack_mode_enabled | out-of-scope | no handler | unhandled |
| serviceGatewayMgr | qa_disable_hijack_mode | out-of-scope | no handler | unhandled |
| serviceGatewayMgr | qa_enable_hijack_mode | out-of-scope | no handler | unhandled |
| envsdkGateway | CheckMessageValid | out-of-scope | no handler | unhandled |
| envsdkGateway | CheckNameValidEx | out-of-scope | no handler | unhandled |
| envsdkGateway | CheckSearchValid | out-of-scope | no handler | unhandled |
| cache | Rowset | out-of-scope | no handler | unhandled |

## Daily goals

These public gateway methods are claimed by `crates/server/lua/daily_goals.lua` and execute the scoped native daily-goal operation. They require an authenticated account and its online character.

| service | method | status | answer |
|---|---|---|---|
| `eve_public.dailygoal.api` | `GetAllCurrentRequest` | handled | Current goals and progress |
| `eve_public.dailygoal.api` | `GetRequest` | handled | One goal by UUID |
| `eve_public.dailygoal.api` | `GetAllWithRewardsRequest` | handled | Paged reward history |
| `eve_public.dailygoal.api` | `RedeemRequest` | handled | Atomically redeem eligible rewards |
| `eve_public.dailygoal.api` | `PayForCompletionRequest` | handled | Pay to complete an eligible current ordinary goal |
| `eve_public.dailygoal.api.admin` | `ProgressRequest` | handled | Authorized, audited QA progress |

QA requires the dedicated account allowlist, a freshly checked QA role, ownership and an online matching session; its original request UUID identifies the audit and replay. A malformed QA correlation UUID returns 400 before Lua. Retired `RedeemAllRequest` and `SubscribeRequest`, and unknown methods in this namespace, return 501 without a default fallback. An authenticated request when the required runtime is unavailable returns 500, not an empty success.

`CurrentGoalsNotice`, `ProgressedNotice`, `CompletedNotice` and `RedeemedNotice` are best-effort after commit; reconnect and Get recover committed state. Actual subscription-grade changes refresh qualification, and online rollover is bounded. Contribution metadata is preserved from the authored source: Type and Identity matchers retain their distinct branches and descriptor-defined checked integer widths, rather than being flattened or silently truncated.

## Ship adaptation

The shipped Lua package claims five exact methods when adaptation content is configured. Replies use descriptor-backed protobuf text, not JSON. Reads do not create a subject. Retained historical user balances remain readable when adaptation content is unavailable.

| service | method | status | handler | answer |
|---|---|---|---|---|
| shipAdaptionMgr | fetch_tree_data | handled | `crates/server/lua/ship_adaption_mgr.lua` | Published tree identity notification; unknown tree refused |
| shipAdaptionMgr | fetch_missions | handled | `crates/server/lua/ship_adaption_mgr.lua` | Inactive mission notification |
| shipAdaptionMgr | fetch_user_data | handled | `crates/server/lua/ship_adaption_mgr.lua` | Identity and zero-filled known chip balances |
| shipAdaptionMgr | InjectChips | handled | `crates/server/lua/ship_adaption_mgr.lua` | Consume an eligible physical injector and credit its attribute-defined virtual currency |
| shipAdaptionMgr | call_chip_exchange | handled | `crates/server/lua/ship_adaption_mgr.lua` | Debit quoted chips and credit LP |
| shipAdaptionMgr | call_finish_node_mission, call_assign_mission, call_unlock_node | fixed | `crates/server/src/services/ship_adaption/` | Unsupported, no effect |

Successful writes return `None`; injection sends `OnItemChange` then `OnUserResponsed`, exchange sends `OnLPChange`, `OnUserResponsed`, then `OnChipExchangeSuccessed`. Balances, including exhausted zero rows, survive restart. Client source exposes an ordinary injector menu; the normal exchange-window entry is unproved, and write-side client cache refresh is not guaranteed. The matching build has no injector or positive acquisition path here for small, medium and large currencies; pre-existing quoted balances remain readable and exchangeable. Node, task and effect gameplay is not implemented; there is no ISK, skill, SP or bonus grant.

## World task and event catalogue

These fixed compatibility boundaries do not implement gameplay. World tasks are
closed in global configuration; forced-open panels see zero data and may poll
user data once per second because the recovery timestamp is zero.

| service | method | status | handler file | answer |
|---|---|---|---|---|
| worldTaskMgr | RequestAllTask | fixed | `crates/server/src/services/world_tasks.rs` | None and empty task notification |
| worldTaskMgr | RequestUserData | fixed | `crates/server/src/services/world_tasks.rs` | None and zero user-data notification |
| worldTaskMgr | ActiveTask | fixed | `crates/server/src/services/world_tasks.rs` | Unsupported error |
| worldTaskMgr | AbortedTask | fixed | `crates/server/src/services/world_tasks.rs` | Unsupported error |
| worldTaskMgr | Search | fixed | `crates/server/src/services/world_tasks.rs` | Unsupported error |
| worldTaskMgr | Select | fixed | `crates/server/src/services/world_tasks.rs` | Unsupported error |
| worldTaskMgr | SubmitItems | fixed | `crates/server/src/services/world_tasks.rs` | Unsupported error |
| worldTaskMgr | Complete | fixed | `crates/server/src/services/world_tasks.rs` | Unsupported error |
| worldTaskContentMgr | GetDungeonShipRestrictions | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| taleMgr | get_active_tales_by_template | fixed | `crates/server/src/services/fixed.rs` | Empty list |
| taleMgr | GetGlobalWorldEventTales | fixed | `crates/server/src/services/fixed.rs` | Empty list |
| custom_agency_provider | get_content_data | fixed | `crates/server/src/services/fixed.rs` | Empty list |
| custom_agency_provider | warp_to_content | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |

## Operations tutorial

These fixed compatibility answers do not implement tutorial gameplay.

| service | method | status | handler file | answer |
|---|---|---|---|---|
| operationsManager | get_character_progress | fixed | `crates/server/src/services/fixed.rs` | Empty dictionary |
| operationsManager | get_active_category_id | fixed | `crates/server/src/services/fixed.rs` | None |
| operationsManager | get_operations_completed_at_least_once | fixed | `crates/server/src/services/fixed.rs` | Empty list |
| operationsManager | get_mission_avoidance_systems | fixed | `crates/server/src/services/fixed.rs` | Empty list |
| operationsManager | skip_task | fixed | `crates/server/src/services/fixed.rs` | False |
| operationsManager | activate_operation_in_solar_system | fixed | `crates/server/src/services/fixed.rs` | False |
| operationsManager | is_category_complete | fixed | `crates/server/src/services/fixed.rs` | False |
| operationsManager | can_character_play_the_tutorial | fixed | `crates/server/src/services/fixed.rs` | False |
| operationsManager | get_tutorial_state | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | is_main_tutorial_finished | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | has_skipped_tutorial | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | start_site | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | complete_category_for_character | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | block_task | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | unblock_task | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | get_category_state | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| operationsManager | process_client_event | fixed | `crates/server/src/services/fixed.rs` | Unsupported error |
| keeper | GetOperationSpawnpointSolarSystem | fixed | `crates/server/src/services/fixed.rs` | None |
| keeper | IsOperationSite | fixed | `crates/server/src/services/fixed.rs` | False |
| keeper | GetLevelEditor | out-of-scope | `crates/server/src/services/fixed.rs` | Unsupported error |
| beyonce | GetOperationSpawnpointPosition | fixed | `crates/server/src/services/ship/beyonce.rs` | Unsupported error |

Initialization reads empty progress and no active category; the client therefore
does not request starting a site or unblocking its first active operation task.
Unsupported errors are refusals, not verified successful protocol responses.
AIR remains disabled and its separate skipped-state behavior is unchanged.
The editor refusal returns no editor object, so its subsequent object binding is
unreachable. Tutorial scenes and their gameplay remain unsupported.

## Node graph QA

Server graph QA tools are out of scope. Server graph execution remains unsupported. Subscription returns no active graphs,
cleanup succeeds without state, and controls explicitly refuse execution.

| service | method | status | handler file | answer |
|---|---|---|---|---|
| node_graph | qa_subscribe | out-of-scope | `crates/server/src/services/npe/node_graph.rs` | Empty dictionary |
| node_graph | qa_unsubscribe | out-of-scope | `crates/server/src/services/npe/node_graph.rs` | None |
| node_graph | qa_start_node_graph | out-of-scope | `crates/server/src/services/npe/node_graph.rs` | Unsupported error |
| node_graph | qa_stop_node_graph | out-of-scope | `crates/server/src/services/npe/node_graph.rs` | Unsupported error |
| node_graph | qa_start_node | out-of-scope | `crates/server/src/services/npe/node_graph.rs` | Unsupported error |
| node_graph | qa_stop_node | out-of-scope | `crates/server/src/services/npe/node_graph.rs` | Unsupported error |
| node_graph | qa_stop_active_nodes | out-of-scope | `crates/server/src/services/npe/node_graph.rs` | Unsupported error |

## Achievement definitions

| service | method | status | handler file |
|---|---|---|---|
| `eve_public.achievement.definition.api.cdn` | `GetLatestRequest` | handled | `crates/server/src/gateway/achievements/definition.rs` |

The public gateway's achievement definition checkpoint points to the unchanged
retail content at `/content/achievements`, served identically for GET and HEAD
(HEAD omits the body). Version and CRC are computed once
when the verified catalogue loads, not per request. This does not implement
achievement progress or rewards.

## Character selection

| service | method | status | handler file |
|---|---|---|---|
| charUnboundMgr | GetQAStarterSystemIDs | fixed | `crates/server/src/services/fixed.rs` |
| charUnboundMgr | GetCharacterInfo | fixed | `crates/server/src/services/fixed.rs` |

The QA starter-system override list is empty; normal starter-system allocation is unchanged. The legacy petition window offers no character choices before character selection. Support submissions are refused and categories are empty.

## Planetary compatibility

| service | method | status | handler file |
|---|---|---|---|
| planetMgr | GetPlanetsForChar | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetMyLaunchesDetails | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetFullNetworkForOwner | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetCommandPinsForPlanet | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetExtractorsForPlanet | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetPlanetInfo | handled | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetPlanetResourceInfo | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetResourceData | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | UserAbandonPlanet | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | UserUpdateNetwork | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMGetLocalDistributionReport | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | UserLaunchCommodities | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | UserTransferCommodities | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GetProgramResultInfo | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMForceInstallProgram | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMConvertCommandCenter | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMAddCommodity | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMGetSynchedServerState | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMRunDepletionSim | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMGetCompleteResource | fixed | `crates/server/src/services/planet_mgr.rs` |
| planetMgr | GMCreateNuggetLayer | fixed | `crates/server/src/services/planet_mgr.rs` |

Planet metadata is loaded from the SDE. Colony and resource reads return empty compatibility values, not simulated colonies or fabricated resources; gameplay actions are refused. Planetary gameplay remains unsupported.

## Station

| service | method | status | handler file |
|---|---|---|---|
| stationSvc | GetStationsForOwner | handled | `crates/server/src/services/station/station_svc.rs` |
| stationSvc | GetStation | handled | `crates/server/src/services/station/station_svc.rs` |
| stationSvc | GetSolarSystem | handled | `crates/server/src/services/station/station_svc.rs` |
| stationSvc | GetAllianceSystems | handled | `crates/server/src/services/station/station_svc.rs` |
| stationSvc | GetSystemsForAlliance | handled | `crates/server/src/services/station/station_svc.rs` |
| station | GetGuests | handled | `crates/server/src/services/character/station.rs` |

NPC station and solar-system data comes from static data; alliance-system rows come from committed sovereignty.

## Sovereignty

| service | method | status | handler file |
|---|---|---|---|
| sovMgr | GetSystemSovereigntyInfo | Lua claim | `crates/server/lua/sovereignty_rpc.lua` |
| sovMgr | GetInfrastructureHubInfo | Lua claim | `crates/server/lua/sovereignty_rpc.lua` |
| sovMgr | GetSovStructuresInfoForSolarSystem | Lua claim | `crates/server/lua/sovereignty_rpc.lua` |
| sovMgr | GetSovStructuresInfoForLocalSolarSystem | Lua claim | `crates/server/lua/sovereignty_rpc.lua` |
| allianceRegistry | GetAllianceSovereigntyStructuresInfo | Lua claim | `crates/server/lua/sovereignty_rpc.lua` |
| allianceRegistry | GetCapitalSystemInfo | Lua claim | `crates/server/lua/sovereignty_rpc.lua` |

These exact claims use the typed sovereignty snapshot and shared Rust wire encoder. The legacy Hub info describes a claim, so a neutral physical Hub returns `None` there while remaining in the structure list. Alliance structure results contain the two legacy projections of the same physical Hub, not two structures. Map and station reads compose committed sovereignty; unsupported indices and campaigns are not fabricated.

## Structure compatibility

| service | method | status | handler file |
|---|---|---|---|
| structureSettings | CharacterGetServices | fixed | `crates/server/src/services/fixed.rs` |
| structureGuests | GetGuests | fixed | `crates/server/src/services/fixed.rs` |
| structureHangarViewMgr | GetMyHangarViewState | fixed | `crates/server/src/services/fixed.rs` |

Structure service and guest reads return empty mappings. The hangar view returns default display attributes, not actual structure health or timer state. Player structure gameplay and the remaining structure methods are deferred.

## RW loyalty-point stations

| service | method | status | handler file |
|---|---|---|---|
| RWManager | get_closest_rw_stations | fixed | `crates/server/src/services/fixed.rs` |

The nearest-station mapping is empty because RW stations and gameplay are not modelled. Ordinary NPC LP station lookup and LP balances are unaffected; RW gameplay remains deferred.

## Map compatibility

| service | method | status | handler file |
|---|---|---|---|
| map | GetInsurgencySystem | fixed | `crates/server/src/services/fixed.rs` |
| map | GetAllRoamingWeatherSystems | fixed | `crates/server/src/services/fixed.rs` |
| map | GetTriglavianMinorVictorySystems | fixed | `crates/server/src/services/fixed.rs` |
| map | GetEdencomMinorVictorySystems | fixed | `crates/server/src/services/fixed.rs` |
| map | GetEdencomFortressSystems | fixed | `crates/server/src/services/fixed.rs` |
| insurgencySolarsystem | GetAllVisibleCampaigns | fixed | `crates/server/src/services/fixed.rs` |

These replies are empty because insurgency, roaming weather, minor-victory and fortress systems, and campaign snapshots are not modelled. Each has the shape its client reader expects: a list for the insurgency membership check, a dictionary for roaming weather, and lists for the others.

## Faction warfare and shipcasters

| service | method | status | handler file |
|---|---|---|---|
| fwWarzoneSolarsystem | GetHQSystemIDs | handled | `crates/server/src/services/station/fw_warzone_solarsystem.rs` |
| fwVictoryPointMgr | GetLocalVictoryPointState, GetAllVictoryPointStates | fixed | `crates/server/src/services/fw_victory_point_mgr.rs` |
| fwCharacterEnlistmentMgr | CreateMyDirectEnlistment, RemoveMyDirectEnlistment | fixed | `crates/server/src/services/agency/fw_character_enlistment_mgr.rs` |
| map | GetFacWarZoneInfo | fixed | `crates/server/src/services/fixed.rs` |
| facWarMgr | GetStats_TopAndAllKillsAndVPs, GetStats_Character, GetStats_Corp, GetStats_Alliance | fixed | `crates/server/src/services/fixed.rs` |
| shipCasterLandingPadMgr | GetFactionLandingPads, GetAllLandingPads | fixed | `crates/server/src/services/fixed.rs` |
| shipCasterLauncherMgr | GetFactionsWithShipcaster | fixed | `crates/server/src/services/fixed.rs` |

Direct enlistment and leaving return explicit unavailable errors; corporation whitelist operations remain available. As server policy, headquarters locations use each warfare faction's static-data home system, not official warzone HQ data. Victory-point reads return compatibility empty states, shipcaster lists are empty, and the closed warzone has zero points with a nonzero maximum. These compatibility answers and refusals do not implement faction warfare: occupation, rewards and shipcaster travel are not implemented.

## Surveys and PVP filaments

| service | method | status | handler file |
|---|---|---|---|
| survey | ClaimSurveyRewards | fixed: none | `crates/server/src/services/fixed.rs` |
| survey | PerformSurveyChecks | fixed: none | `crates/server/src/services/fixed.rs` |
| survey | GetActiveSurveyID | fixed: none | `crates/server/src/services/fixed.rs` |
| pvpFilamentMgr | GetAllEvents | fixed: empty dictionary | `crates/server/src/services/fixed.rs` |
| pvpFilamentMgr | GetActiveEvents | fixed: empty dictionary | `crates/server/src/services/fixed.rs` |
| pvpFilamentMgr | GetMostRecentEvent | fixed: none | `crates/server/src/services/fixed.rs` |
| pvpFilamentMgr | GetNextEventDate | fixed: none | `crates/server/src/services/fixed.rs` |
| pvpFilamentMgr | JoinPVPQueue | unavailable | `crates/server/src/services/fixed.rs` |
| pvpFilamentMgr | LeavePVPQueue | unavailable | `crates/server/src/services/fixed.rs` |

Surveys and PVP events are closed, without reward grants, event persistence or queues. `GetLeaderboard` and `GetCharacterStatistics` are deferred and unhandled: the client cannot request event details without event data.

## Travel and amplification matrix compatibility

| service | method | status | handler file |
|---|---|---|---|
| voidSpaceMgr | ClientIsReady | fixed readiness acknowledgement | `crates/server/src/services/fixed.rs` |
| voidSpaceMgr | VoidSpacePlayerJump | unavailable | `crates/server/src/services/fixed.rs` |
| solarsystemInterferenceMgr | GetLocalInterferenceState, GetAllInterferenceBands | fixed closed display | `crates/server/src/services/fixed.rs` |
| starterShipcasterTravelMgr | CanCharacterJump | unavailable | `crates/server/src/services/fixed.rs` |
| randomJumpMgr | ActivateRandomJumpFilament | unavailable | `crates/server/src/services/fixed.rs` |
| amplificationMatrixSolarsystemMgr | FetchMatrixPacked | empty packed notification | `crates/server/src/services/amplification_matrix.rs` |
| amplificationMatrixSolarsystemMgr | ActiveSkill, InactiveSkill, SyncMatrixDbuff, TransferCash | unavailable | `crates/server/src/services/amplification_matrix.rs` |

The interference reply is a display-only quiescent state, not travel authorization. Matrix fetch reports two empty slots only to the requester; no matrix activation or deposit is performed.

## Encounter Surveillance System

| service | method | status | handler file |
|---|---|---|---|
| dynamicResourceCacheMgr | GetDBSMapData | fixed | `crates/server/src/services/fixed.rs` |
| dynamicResourceCacheMgr | GetESSAgencyData | fixed | `crates/server/src/services/fixed.rs` |
| dynamicResourceCacheMgr | GetESSReserveBankKeys | fixed | `crates/server/src/services/fixed.rs` |
| dynamicResourceCacheMgr | GetESSSystemDetails | fixed | `crates/server/src/services/fixed.rs` |
| dynamicResourceCacheMgr | GetDynamicResourceSettings | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | GetDataForClientSolarSystem | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | GetMainBankTheftsForClientSolarSystem | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | GetReserveBankTheftsForClientSolarSystem | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | IsClientLinkedToReserveBank | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | AttemptLinkToMainBank | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | AttemptLinkToReserveBank | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | RequestMainBankUnlink | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | RequestReserveBankUnlink | fixed | `crates/server/src/services/fixed.rs` |
| essMgr | RequestUnlockReserveBank | fixed | `crates/server/src/services/fixed.rs` |
| dynamicBountyMgr | GetOutputForClientSolarSystem | fixed | `crates/server/src/services/fixed.rs` |

ESS (closed; full ESS is future game content) provides empty agency and map data, keys and histories, inactive system details, and refused link and unlock operations without changes. Settings are inert server policy rather than official gameplay values: durations and divisors are positive, while allocation and payout are zero. No banks, links, payouts, notifications or persistent ESS state are implemented. Dynamic bounty output remains inactive.

## Fighters and positional actions

| service | method | status | handler file |
|---|---|---|---|
| fighterMgr | GetFightersForShip | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | ExecuteMovementCommandOnFighters | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | LoadFightersToTube | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | UnloadTubeToFighterBay | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | LaunchFightersFromTubes | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | RecallFightersToTubes | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | CmdAbandonFighter | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | CmdScoopAbandonedFighterFromSpace | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | CmdActivateAbilitySlots | fixed | `crates/server/src/services/fixed.rs` |
| fighterMgr | CmdDeactivateAbilitySlots | fixed | `crates/server/src/services/fixed.rs` |
| warpVectorMgr | UseVector | fixed | `crates/server/src/services/fixed.rs` |
| superWeaponMgr | ActivateSinglePointTargetedModule | fixed | `crates/server/src/services/fixed.rs` |
| superWeaponMgr | ActivateSlashModule | fixed | `crates/server/src/services/fixed.rs` |

These are deferred compatibility answers, not gameplay: the snapshot contains empty tube and space lists and an empty ability-state dictionary; each action is refused without changing state. `GetFighterAbilityRearmPerChargeBonusTime` remains unhandled because an empty snapshot supplies no controlled fighters and the client falls back to local static data.

## Mining ledger

| service | method | status | handler file |
|---|---|---|---|
| corpMiningLedger | GetObserversWithMiningEvents | fixed | `crates/server/src/services/fixed.rs` |
| corpMiningLedger | GetObserverLedger | fixed | `crates/server/src/services/fixed.rs` |

Corporate mining observers and structure ledger history are unsupported; their queries return empty lists. Personal mining history is unchanged.

## Moon mining and structure compression

| service | method | status | handler file |
|---|---|---|---|
| moonExtractions | GetExtractionAndEventIDFromStructureID | fixed | `crates/server/src/services/fixed.rs` |
| moonExtractions | GetExtractionsForCorp | fixed | `crates/server/src/services/fixed.rs` |
| moonExtractions | GetMoonResourcesForStructure | fixed | `crates/server/src/services/fixed.rs` |
| moonExtractions | StartNewExtraction | fixed | `crates/server/src/services/fixed.rs` |
| moonExtractions | FractureChunkWithStructure | fixed | `crates/server/src/services/fixed.rs` |
| moonExtractions | CancelExtraction | fixed | `crates/server/src/services/fixed.rs` |
| structureCompressionMgr | DecompressGasInStructure | fixed | `crates/server/src/services/fixed.rs` |
| structureCompressionMgr | GetMyGasDecompressionEfficiency | fixed | `crates/server/src/services/fixed.rs` |
| structureCompressionMgr | CompressItemInStructure | fixed | `crates/server/src/services/fixed.rs` |
| autoMoonMining | RequestMiningDetails | fixed | `crates/server/src/services/fixed.rs` |
| autoMoonMining | RequestMiningCycleOutput | fixed | `crates/server/src/services/fixed.rs` |

Extraction reads return an absent extraction and event, an empty list, or an empty resource dictionary. Extraction actions and structure compression and efficiency requests are refused. Automatic miner details are refused so the window closes; cycle output is an empty dictionary. No extraction, compression or automatic drilling is performed.

## Starbases

| service | method | status | handler file |
|---|---|---|---|
| posMgr | GetControlTowers | fixed | `crates/server/src/services/fixed.rs` |
| posMgr | GetTowerSentrySettings | fixed refusal | `crates/server/src/services/fixed.rs` |
| posMgr | GetTowerNotificationSettings | fixed refusal | `crates/server/src/services/fixed.rs` |
| posMgr | GetStarbasePermissions | fixed refusal | `crates/server/src/services/fixed.rs` |
| posMgr | GetMoonForTower | fixed refusal | `crates/server/src/services/fixed.rs` |
| posMgr | GetConsumptionFactor | fixed refusal | `crates/server/src/services/fixed.rs` |

The tower list is empty. Tower-specific settings, permissions, moon and fuel factor requests return a readable refusal; no tower state is created. The starbase window and its conditional tabs have not been exercised with a real client.

## Fleets

| service | method | status | handler file |
|---|---|---|---|
| fleetObjectHandler | Reconnect | handled | `crates/server/src/services/fleets/fleet_object_handler.rs` |
| beyonce | GetFleetLocationAndInfo | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | GetFleetState | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | CmdFleetTagTarget | handled | `crates/server/src/services/ship/beyonce.rs` |
| sovDashboard | GetCynoJammerState | fixed | `crates/server/src/services/fixed.rs` |

A disconnected member's fleet seat is kept for `fleets.reconnect_timeout_secs` (120 seconds by default, at most 120) so the client can rejoin automatically; seats do not survive a server restart.

Fleet location queries include online, visible fleet members in the caller's current system. Fleet commanders (the fleet leader, wing commanders and squad commanders) can set or clear target tags for visible targets; members receive their current visible state and explicit clears. Fleet tags are held in process memory. Cynosural jamming is not modeled, so the dashboard reports no interference.

## Ship

| service | method | status | handler file |
|---|---|---|---|
| ship | Undock | handled | `crates/server/src/services/ship/ship.rs` |
| ship | Board | handled | `crates/server/src/services/ship/boarding.rs` |
| ship | StoreVessel | handled | `crates/server/src/services/ship/stored.rs` |
| ship | BoardStoredShip | handled | `crates/server/src/services/ship/stored.rs` |
| ship | LaunchFromContainer | handled | `crates/server/src/services/ship/stored.rs` |
| ship | AssembleShip | handled | `crates/server/src/services/fitting/ship.rs` |
| dogmaIM | CreateNewbieShip, CreateNewbieBonusItems | handled | `crates/server/src/services/fitting/newbie.rs` |
| ship | CheckEnoughSubSystemToUndock | fixed | `crates/server/src/services/ship/ship.rs` |
| ship | GetDirtTimestamp, ResetDirtTimestamp | handled | `crates/server/src/services/ship/cleaning.rs` |
| ship | LeaveShip, SetShipPassword | refused | `crates/server/src/services/ship/ship.rs` |
| dogmaIM | AddTargetOBO, RemoveTargetOBO | refused | `crates/server/src/services/dogma/dogma_im.rs` |
| ship | GetShipFittingInfo | fixed refusal | `crates/server/src/services/ship/ship.rs` |
| ship | GetShipConfiguration | handled | `crates/server/src/services/ship/configuration.rs` |
| ship | ConfigureShip | handled | `crates/server/src/services/ship/configuration.rs` |
| ship | LaunchDrones | handled | `crates/server/src/services/drones/inventory/launch.rs` |
| ship | ScoopDrone | handled | `crates/server/src/services/drones/inventory/returning.rs` |

The rookie ship request boards a newly created racial rookie ship when the current ship is not already a rookie ship, preserving the previous hull and its contents. Independently, the bonus request grants the six entries of the server-selected racial rookie fit once per character as loose items in the current NPC station hangar. Both use the same embedded catalogue as character provisioning, refuse structure and in-space requests, and return `None` after commit. Actual-client button and hangar-refresh behavior is NOT VERIFIED.

The dirt RPCs take exactly one positive ship ID and require the current location binding. Reads initialize a persistent timestamp for a local physical hull, including visible foreign hulls in space without a fleet or corporation affiliation. Reset applies to an owned singleton ship in the current NPC station hangar, active or inactive, or the current piloted ship in space. It returns the committed FILETIME without a separate notification. This describes the service protocol; the real client clean-button UI has not been verified.

`ConfigureShip` accepts a dictionary patch of the four boolean keys `FleetHangar_AllowFleetAccess`, `FleetHangar_AllowCorpAccess`, `SMB_AllowFleetAccess` and `SMB_AllowCorpAccess` for the account's currently selected owned ship. It requires the current location-bound ship moniker, in station or space; an unknown key or non-boolean value refuses the entire patch. Settings are owner-bound, default to false and do not carry a previous owner's grants to a new owner. `GetShipConfiguration(shipID)` returns all four keys for an owned ship in the current station hangar or a visible owned ship in space, and for a currently fleet- or corporation-affiliated observer of a visible ship in space. This metadata is available before any switch grants bay contents and without an operational-range check. Foreign docked, unaffiliated or invisible ships are refused. The client's 15-second metadata cache is not an authorization grant.

In space the ship service binds to `(system, GROUP_SOLAR_SYSTEM)`; station configuration uses the current station binding. `StoreVessel(hostID)`, `BoardStoredShip(hostID, shipID)` and `LaunchFromContainer(hostID, itemID)` use current maintenance-bay access and the ship transitions described in [Features](features.md). Store and board return the server's existing ship-state shape; launch returns an empty mutator result. These are server choices, not captured retail response shapes: client wrappers ignore the results. Fleet hangar and maintenance bay opening uses local client wrappers and bound inventory calls, not separate `Open*` remote methods. These checks concern ship and shared-inventory operations, not blanket authorization for other services.

After actor restoration, recovery preserves normal motion of the old unpiloted hull while fencing persistent membership, type, owner and the absence of a pilot.

For a real-client walkthrough, exercise all four switches, store, board and launch, and closed, full, private-inner-hull and out-of-range cases. This walkthrough requires an executable client; server tests are not a substitute for it.

## Navigation

| service | method | status | handler file |
|---|---|---|---|
| beyonce | CmdGotoPoint | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | CmdGotoBookmark | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | CmdAlignTo | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | CmdWarpToStuff | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | CmdWarpToStuffAutopilot | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | CmdFleetRegroup | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | AdminGetBubbleID | handled | `crates/server/src/services/ship/beyonce.rs` |
| beyonce | GetRelativity | handled | `crates/server/src/services/ship/beyonce.rs` |

`CmdGotoPoint` flies the ship towards finite coordinates. `CmdGotoBookmark`, `CmdAlignTo` with `bookmarkID` and `CmdWarpToStuff('bookmark', ...)` use the coordinates stored with the bookmark: the pilot's own bookmark, or one in a shared folder with use, manage or admin access. A view-only, expired, missing or other-system bookmark is refused without movement, and coordinates sent by the client are never used. A bookmark warp can be a fleet warp. `CmdWarpToStuffAutopilot` takes only a celestial or object ID and never reads it as a bookmark. `CmdWarpToStuff` refuses a kind of destination it has no handler for instead of reading its ID as an object. `CmdFleetRegroup` sends the commander's subordinates in the same system who allow regrouping towards the commander's position, without warping; the client does not define the movement, so this is the server's own rule. `AdminGetBubbleID` returns the current solar system ID to a GM, because the server keeps one ballpark per system. `GetRelativity` returns the distance from the ship to a point, the ship's position, the system's current destiny tick and the server's current FILETIME.

Each outgoing destiny notification contains events for one logical stamp, preserving their original order within that stamp.

## External phone binding

| service | method | status | handler file |
|---|---|---|---|
| mobileCollectMgr | MobileBindSucceed | fixed | `crates/server/src/services/fixed.rs` |

The callback receives a fixed `None` reply only to complete the call; it does not verify or persist a binding. Phone binding, binding state and rewards remain unsupported.

## Wallet

| service | method | status | handler file |
|---|---|---|---|
| account | GetCashBalance | handled | `crates/server/src/services/account.rs` |
| account | GetWalletDivisionsInfo | handled | `crates/server/src/services/account.rs` |
| account | GetEntryTypes | handled | `crates/server/src/services/account.rs` |
| account | GetTransactions | handled | `crates/server/src/services/account.rs` |
| account | GiveCash | handled | `crates/server/src/services/account.rs` |
| account | GiveCashFromCorpAccount | handled | `crates/server/src/services/account.rs` |
| account | GetJournal | fixed | `crates/server/src/services/fixed.rs` |
| account | GetJournalForAccounts | fixed | `crates/server/src/services/fixed.rs` |

`GetTransactions` returns the selected wallet's journal rows for the last 30 days or a calendar month. `GetCashBalance` and `GetTransactions` refuse a corporation division with the client's access error to members without the read right, and refuse an invalid division instead of falling back to the master wallet. `GetWalletDivisionsInfo` lists all seven divisions and gives a balance of 0 for each one the member may not read. `GetJournal` and `GetJournalForAccounts` return empty lists.

## Contracts

| service | method | status | handler file |
|---|---|---|---|
| contractProxy | GM_ExpireContract | handled | `crates/server/src/services/contracts.rs` |

Authorized GMs can advance outstanding deadlines. Auctions with bids settle normally;
other contracts retain their escrow for issuer reclamation.

## Shared settings

| service | method | status | handler file |
|---|---|---|---|
| sharedSettingsMgr | StoreSettingLinkAndGetID | handled | `crates/server/src/services/settings_sharing/shared_settings_mgr.rs` |
| sharedSettingsMgr | GetStoredSharedSetting | handled | `crates/server/src/services/settings_sharing/shared_settings_mgr.rs` |

Authenticated players can forward permanent item-filter and fleet-broadcast snapshots to any other signed-in player. Links are not private and cannot be revoked or expired. Payloads use JSON as a YAML subset, with a lowercase SHA-256 digest of the exact encoded UTF-8 bytes.

## Exploration

| service | method | status | handler file |
|---|---|---|---|
| scanMgr | GetSystemScanMgr | handled | `crates/server/src/services/exploration/scan_mgr.rs` |
| scanMgr | QAScanSites | handled | `crates/server/src/services/exploration/scan_mgr.rs` |

QA-role accounts can select exploration targets for full-certainty results. Signature authorization persists per character and is shared with ordinary scanning and warp and bookmark checks.

## Missions and agents

| service | method | status | handler file |
|---|---|---|---|
| epicArcStatus | AgentHasEpicMissionsForCharacter | handled | `crates/server/src/services/missions/epic_arc_status.rs` |
| agentMgr | GetPlausibleMissionIDs | fixed | `crates/server/src/services/fixed.rs` |
| agentMgr | GetMessagesForEpicArcMissions | handled | `crates/server/src/services/missions/agent_mgr.rs` |

Epic-arc availability follows the selected character's active missions and unlocked progress; replay support is unchanged. The QA mission-selection tool is unavailable and receives no candidates.

## Mailing lists

| service | method | status | handler file |
|---|---|---|---|
| mailingListsMgr | Create | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | Join | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | GetInfo | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | GetJoinedLists | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | GetMembers | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | GetSettings | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | GetWelcomeMail | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | Leave | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | Delete | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | KickMembers | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | SetMembersMuted | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | SetMembersOperator | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | SetMembersClear | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | SetEntitiesAccess | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | ClearEntityAccess | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | SetDefaultAccess | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | SaveWelcomeMail | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | SendWelcomeMail | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailingListsMgr | ClearWelcomeMail | handled | `crates/server/src/services/mail/mailing_lists_mgr.rs` |
| mailMgr | SendMail | handled | `crates/server/src/services/mail/mail_mgr.rs` |
| mailMgr | MarkAsReadByList | handled | `crates/server/src/services/mail/mail_mgr.rs` |
| mailMgr | MarkAsUnreadByList | handled | `crates/server/src/services/mail/mail_mgr.rs` |
| mailMgr | MoveToTrashByList | handled | `crates/server/src/services/mail/mail_mgr.rs` |

## Skills

| service | method | status | handler file |
|---|---|---|---|
| dogmaIM | InjectSkillIntoBrain | handled | `crates/server/src/services/skills/books.rs` |
| skillHandler | CharStartTrainingSkill | handled | `crates/server/src/services/skills/books.rs` |
| skillHandler | SaveNewQueue | handled | `crates/server/src/services/skills/training.rs` |
| skillHandler | PurchaseSkills | handled | `crates/server/src/services/skills/purchase.rs` |
| nonDiminishingInjectionMgr | GetAvailableNonDiminishingInjections | handled | `crates/server/src/services/skills/non_diminishing_injection_mgr.rs` |

Plain `InjectSkillIntoBrain` consumes an accessible published skill book and learns the skill at level zero without checking prerequisites. `CharStartTrainingSkill` uses the same injection transaction, then validates the resulting level-one queue entry through `SaveNewQueue`; unmet prerequisites return the native queue refusal and roll back the book, skill, history and queue changes. The bound skill handler returns the bought type IDs in request order and sends the same committed skill updates as skillbook injection. Each charged direct skill purchase writes a wallet journal entry in the same transaction; free skills write no entry. Skills in the client's hidden skill groups cannot be bought.

`GetAvailableNonDiminishingInjections` reads the selected character's active injector credits and returns zero without a database read when no character is selected. The client caches the result and updates it from injector notifications.

## Multi-character training

| service | method | status | handler file |
|---|---|---|---|
| userSvc | GetUpdatedMultiCharacterTraining | fixed | `crates/server/src/services/fixed.rs` |
| userSvc | ActivateMultiTraining | fixed | `crates/server/src/services/fixed.rs` |
| userSvc | ActivateSoulboundMultiTraining | fixed | `crates/server/src/services/fixed.rs` |

Each character has an independent skill queue. The training window receives an empty list; both item and soulbound activation are refused without consuming anything. Cache invalidation only acknowledges the request.

## Account and platform boundaries

| service | method | status | handler | answer |
|---|---|---|---|---|
| subscriptionMgr | KickAlphaUsersOnTheSameComputer | out-of-scope | no handler | unhandled |
| userSvc | InvalidateMultiCharacterTrainingCache | fixed | `crates/server/src/services/fixed.rs` | None acknowledgement |
| userSvc | GetSessionPlaytimeMinutes | out-of-scope | no handler | unhandled |
| userSvc | ActivateRenameCard | fixed | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| userSvc | ActivateCharacterReSculpt | fixed | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| userSvc | ReverseRedeem | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| userSvc | ReportISKSpammer | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| userSvc | CastUpvote | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| userSvc | CastDownvote | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| userSvc | ReportBot | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| userSvc | CreateRedeemTokenQA | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| communityTrackMgr | change_qa_mode | out-of-scope | `crates/server/src/services/fixed.rs` | CustomNotify refusal |
| characterEnergyMgr | GetMyEnergyState | out-of-scope | no handler | unhandled |
| userSvc | GetUserToken | out-of-scope | no handler | unhandled |
| userSvc | GetUserName | out-of-scope | no handler | unhandled |
| userSvc | GetCreateDate | out-of-scope | no handler | unhandled |
| communityTrackMgr | get_instance_attributes_public | out-of-scope | no handler | unhandled |
| infoGatheringMgr | LogInfoEventsFromClient | out-of-scope | no handler | unhandled |

The same-computer clone check is gated by `monolithconfig.enabled('kick_alpha_users_on_same_computer')`; the server's global configuration does not supply this key, and the client defaults it to false when no other source overrides it. Regional playtime is used by Korean addiction warning login and splash paths; the server sends no country code, so those paths are not enabled. Both calls remain unhandled if forced. Account actions, reporting, votes and QA writes are refused without persisting anything. Energy state, identity, creation date and community track attributes require real data and are not fabricated. Information gathering is disabled by `GetStateAndConfig`; the client sends telemetry only when enabled, and the server does not enable it. A forced telemetry call is unhandled.

## Market

| service | method | status | handler file |
|---|---|---|---|
| marketProxy | PlaceBuyOrder | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | BuyMultipleItems | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | PlaceMultiSellOrder | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | PlacePlexSellOrder | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | ModifyCharOrder | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | ModifyPlexCharOrder | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | CancelCharOrder | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetOrders | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetPlexOrders | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetStationAsks | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetSystemAsks | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetRegionBest | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | StartupCheck | compatibility, no-op | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetCharEscrow | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetPlexBest | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetPlexHistory | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetHistoryForManyTypeIDs | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetOldPriceHistory | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetNewPriceHistory | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetPlexOldPriceHistory | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetPlexNewPriceHistory | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | CharGetTransactions | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | CorpGetTransactions | handled, division-gated | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetCharOrders | handled | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetCorporationOrders | handled, division-gated | `crates/server/src/services/market/market_proxy.rs` |
| marketProxy | GetMarketOrderHistory | handled | `crates/server/src/services/market/market_proxy.rs` |

PLEX orders and quotes use the player-only global book; other types use regional books. Shared placement, cancellation, orders, escrow, transaction and history reads include the applicable PLEX scope. Shared ask and best-price queries include global PLEX quotes. `ModifyCharOrder` refuses global PLEX orders; `ModifyPlexCharOrder` refuses regional orders. Corporate market queries use current division read authority; wallet selection remains in the existing account service.

## Expert systems

| service | method | status | handler file |
|---|---|---|---|
| expertSystemMgr | GetMyExpertSystems | handled | `crates/server/src/services/expert_systems.rs` |
| expertSystemMgr | ConsumeExpertSystem | handled | `crates/server/src/services/expert_systems.rs` |
| expertSystemMgr | RemoveMyExpertSystem | handled | `crates/server/src/services/expert_systems.rs` |

The installed expert systems are returned by type ID with their install and expiry times as FILETIME. Consuming takes an item ID and removing a type ID; both return `None` once the change is saved, and otherwise refuse with a message and change nothing. Every change sends `OnExpertSystemsUpdated` with the full set.

## Store purchases

| service | method | status | handler file |
|---|---|---|---|
| storeManager | get_categories | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | get_products | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | get_offers | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | get_offer_states | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | get_offer_groups | fixed | `crates/server/src/services/store/store_manager.rs` |
| storeManager | buy_offer | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | free_to_get | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | set_offer_purchased | handled | `crates/server/src/services/store/store_manager.rs` |
| vaultManager | get_account_balance | handled | `crates/server/src/services/store/vault_manager.rs` |
| userSvc | GetRedeemTokens | handled | `crates/server/src/services/store/user_svc.rs` |
| userSvc | ClaimRedeemTokens | handled | `crates/server/src/services/store/user_svc.rs` |

The expert system category carries the `expert system` tag. A paid offer delivers consumable items through the redeeming queue; renewing an expert system means buying the same item again and consuming it. The server checks the price and quantity of every paid purchase and saves all of its changes together; the queue and PLEX balance notifications follow the commit, and an offline gift recipient finds the token at the next login. `get_offer_groups` returns an empty list. `set_offer_purchased`, which marks a period offer as bought before an external payment, is refused with a message and changes nothing.

## Clone grade

| service | method | status | handler file |
|---|---|---|---|
| subscriptionMgr | GetCloneGrade | handled | `crates/server/src/services/subscriptions/subscription_mgr.rs` |
| subscriptionMgr | GetSubscriptionTime | handled | `crates/server/src/services/subscriptions/subscription_mgr.rs` |
| loginRewardFacilities | get_my_clone_grade | handled | `crates/server/src/services/store/login_reward_facilities.rs` |
| alphaInjectorMgr | GetNextAvailableInjection | handled | `crates/server/src/services/subscriptions/alpha_injector_mgr.rs` |

The client asks for the grade with and without the grace period through the same call, so both answers include the grace period. The subscription time is the Omega end date in Unix nanoseconds: zero for Alpha, and 31 December 2100 for Omega with no end date. The Alpha injector query returns when the selected character can next use a daily Alpha injector, as a FILETIME, or zero if it never has.

## Player structure directory

| service | method | status | handler file |
|---|---|---|---|
| structureDirectory | GetMyCharacterStructures | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | GetMyCorporationStructures | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | GetStructureMapData | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | GetMyDockableStructures | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | GetSolarSystemsWithBeacons | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | GetNearbyJumpBridges | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | GetJumpBridgesWithMyAccess | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | GetStructureDescription | fixed | `crates/server/src/services/structure_directory.rs` |
| structureDirectory | SetStructureDescription | fixed | `crates/server/src/services/structure_directory.rs` |
| structureSettings | CharacterHasSetting | fixed | `crates/server/src/services/fixed.rs` |
| structureSettings | CharacterHasService | fixed | `crates/server/src/services/fixed.rs` |
| structureSettings | CharacterGetService | fixed | `crates/server/src/services/fixed.rs` |
| structureSettings | CharacterCheckService | fixed | `crates/server/src/services/fixed.rs` |
| structureDeployment | RenameStructure | fixed | `crates/server/src/services/fixed.rs` |
| structureDeployment | Unanchor | fixed | `crates/server/src/services/fixed.rs` |
| structureDeployment | CancelUnanchor | fixed | `crates/server/src/services/fixed.rs` |
| structureDeployment | Anchor | fixed | `crates/server/src/services/fixed.rs` |
| structureDeliveries | Deliver | fixed | `crates/server/src/services/fixed.rs` |
| structureControl | TakeControl | fixed | `crates/server/src/services/fixed.rs` |
| structureControl | ReleaseControl | fixed | `crates/server/src/services/fixed.rs` |
| structureControl | GetStructurePilot | fixed | `crates/server/src/services/fixed.rs` |
| structureJumpBridgeMgr | GetJbStructureDestination | fixed | `crates/server/src/services/fixed.rs` |
| structureJumpBridgeMgr | GetLinkedStructure | fixed | `crates/server/src/services/fixed.rs` |
| structureDocking | Undock | fixed | `crates/server/src/services/fixed.rs` |
| structureDocking | Dock | fixed | `crates/server/src/services/fixed.rs` |
| structureVulnerability | UpdateReinforceTimesFromClient | fixed | `crates/server/src/services/fixed.rs` |
| structureCargoDelivery | DropOffItems | fixed | `crates/server/src/services/fixed.rs` |
| structureAssetSafety | MoveEjectTimeGM | fixed | `crates/server/src/services/fixed.rs` |
| structureAssetSafety | GetStructuresICanDeliverTo | fixed | `crates/server/src/services/fixed.rs` |
| structureAssetSafety | MoveSafetyWrapToStructure | fixed | `crates/server/src/services/fixed.rs` |
| structureAssetSafety | MovePersonalAssetsToSafety | fixed | `crates/server/src/services/fixed.rs` |
| structureAssetSafety | MoveCorpAssetsToSafety | fixed | `crates/server/src/services/fixed.rs` |
| structureAssetSafety | GetWrapNames | fixed | `crates/server/src/services/fixed.rs` |
| structureCynoBeaconMgr | CmdBridgeToStructureBeacon | fixed | `crates/server/src/services/fixed.rs` |
| structureCynoBeaconMgr | CmdGroupJumpToStructureBeacon | fixed | `crates/server/src/services/fixed.rs` |
| structureProfiles | GetProfiles | fixed | `crates/server/src/services/fixed.rs` |
| structureProfiles | CreateProfile | fixed | `crates/server/src/services/fixed.rs` |

Player structures are unavailable. Directory and profile reads expose empty compatibility state; deployment, asset movement, control, docking and structure travel are explicitly refused. The market's multi-buy window lists only NPC stations.

## Jump clones

| service | method | status | handler file |
|---|---|---|---|
| jumpCloneSvc | GetCloneState | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | GetStationCloneState | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | GetPriceForClone | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | ValidateInstallJumpClone | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | InstallCloneInStation | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | CloneJump | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | DestroyInstalledClone | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | SetJumpCloneName | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | ResetLastCloneJumpTime | handled | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | GetShipCloneState | fixed | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | GetNumClonesInPilotsStructure | fixed | `crates/server/src/services/clones/jump_clone_svc.rs` |
| jumpCloneSvc | InstallCloneInStructure | fixed | `crates/server/src/services/clones/jump_clone_svc.rs` |

Station clones keep their own implants. Ship clone bays and player structures are not implemented yet: the ship clone state is empty, the structure clone count is zero, and installing a clone in a structure is refused with a message.

## Inventory

| service | method | status | handler file |
|---|---|---|---|
| invbroker | GetContainerContents | handled | `crates/server/src/services/inventory/reads.rs` |
| invbroker | ValidateItemListCanBeOpened | handled | `crates/server/src/services/inventory/reads.rs` |
| invbroker | GetDamageForCrystals | handled | `crates/server/src/services/inventory/inv_broker.rs` |
| invbroker | SetLabel | handled | `crates/server/src/services/inventory/inv_broker.rs` |
| invbroker | TrashItems | handled | `crates/server/src/services/inventory/inv_broker.rs` |
| invbroker | WithdrawPlexFromVault | handled | `crates/server/src/services/inventory/plex.rs` |
| invbroker | DepositPlexToVault | handled | `crates/server/src/services/inventory/plex.rs` |
| invInventory | ALSCConfigGet | fixed | `crates/server/src/services/fixed.rs` |
| invInventory | ALSCConfigSet | out-of-scope | `crates/server/src/services/fixed.rs` |
| invInventory | GetPriceForItems | out-of-scope | `crates/server/src/services/fixed.rs` |

Audit log secure containers report that they are not configured; saving their configuration and `GetPriceForItems` are refused.

Both reads require the current station or solar-system broker binding and recheck current access. `GetContainerContents(itemID, dockableLocationID[, solarSystemID])` accepts two or three positional arguments; the optional system may be `None`. The supplied location and system must match the resolved root. It returns direct inventory DBRows, including native tuple-keyed loaded charge rows only for a fully authorized ship; shared-bay access alone does not expose private fittings or charges. An owned assembled ship or container can be inspected remotely at its supported station or system root. Corporation custody requires current scoped query authority, and shared access uses current grants and reachability.

`ValidateItemListCanBeOpened` accepts one list or tuple of canonical inventory DBRows, compares their identity snapshots with current items and local opening access, and returns invalid item IDs. Malformed rows or calls receive a typed refusal rather than an invalid-ID list. These reads do not write items or load or reconcile fittings.

Shared inventory DBRows encode `categoryID` as a signed 32-bit field and
`quantity` as signed 64-bit `I8`. Virtual `stacksize` and `singleton`
declarations and their negative quantity conventions are unchanged.

`charFittingMgr.SaveManyFittings` accepts ordinary wire integers for fitting
quantities, validates the entire batch and saves it in one transaction; a
refusal leaves no partial batch. No `SingleSave` method is added.

PLEX can be withdrawn to, and returned from, the selected character's personal hangar at the current NPC station. Direct-trade withdrawal, structures and other conversion scopes are refused. The completion event follows the response; a withdrawn stack remains in the hangar if a later sale fails.

## Crates

| service | method | status | handler file |
|---|---|---|---|
| crateService | PeekInsideCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | PeekInsideFixedCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | CustomGetLootFromFixedCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | GetCrate | fixed | `crates/server/src/services/crates/crate_service.rs` |
| crateService | GetCrateLocation | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | OpenCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | CustomOpenCrate | fixed | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimLootFromCrateStack | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimLoot | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimSkillBundle | fixed | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimFitting | fixed | `crates/server/src/services/crates/crate_service.rs` |
| skillHandler | GetSkillBundleInfo | fixed | `crates/server/src/services/skills/skill_handler.rs` |
| storeManager | can_purchase_offer | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | is_offer_purchasable | handled | `crates/server/src/services/store/store_manager.rs` |

The three preview calls return the crate's contents as `(typeID, quantity, None)` entries and change nothing; a preview of a crate item needs the same ownership, corporation hangar role and reachability as moving it. `GetCrate` returns an empty list. `OpenCrate` opens one ordinary direct-list crate. `ClaimLootFromCrateStack` opens up to 30 from one stack, all or nothing, and returns `(opened, requested)`; a larger stack opens 30 per call. Rewards are created immediately in the opener's personal hangar at the current station when docked, or in the active ship's cargo in space; the operation is refused if the cargo lacks room. A corporation-owned crate requires hangar Take permission and its rewards go to the opener. Source consumption, reward creation and receipts commit together, followed by notifications.

`GetCrateLocation(itemID)` checks the same source authorization and ordinary-crate eligibility as opening and returns the current reward destination without changing or reserving anything. Opening independently rechecks the current session and inventory.

`ClaimLoot(itemID, destinationID, destinationFlagID)` confirms the receipt recorded at opening only for the character who opened the crate and the exact destination. Repeats succeed, even after the reward is used or merged; the call never creates or moves items or sends notifications. There is no request-keyed replay protection for opening: client call IDs are fresh per call. `CustomOpenCrate`, `ClaimSkillBundle` and `ClaimFitting`, as well as skill-bundle and fitting details, are refused with a message. The store checks answer no for an unknown offer, a daily gift already claimed today, or a crate.

## Item compatibility

| service | method | status | handler file |
|---|---|---|---|
| encodedItems | DecodeItem | fixed | `crates/server/src/services/fixed.rs` |
| itemLocking | GetItemsByLocation | fixed | `crates/server/src/services/fixed.rs` |
| itemLocking | GetLockedItemLocations | fixed | `crates/server/src/services/fixed.rs` |

Encoded-item decoding is deferred and currently returns an explicit refusal without consuming or changing the item. Corporation lockdown management and voting are deferred; locked-item queries retain an empty mapping and a mutable empty set of locations.

## Single-track login campaigns

| service | method | status | handler file |
|---|---|---|---|
| loginCampaignManager | get_client_campaign_state | fixed | `crates/server/src/services/fixed.rs` |
| loginCampaignManager | claim_reward | fixed | `crates/server/src/services/fixed.rs` |

Single-track login campaigns are not open. Both the campaign-state query and claim request return None, meaning no campaign and no reward claimed. Seasonal login campaigns use their separate implementation and are unchanged.

## Seasonal and activity compatibility

| service | method | status | handler file | answer or closed gate |
|---|---|---|---|---|
| seasonManager | get_season_data_for_character | fixed | `crates/server/src/services/fixed.rs` | None; no active season or progress |
| seasonManager | get_character_progress_by_character_id | deferred-no-handler | no handler | No active season and no change notification producer |
| seasonManager | select_season | deferred-no-handler | no handler | No season selection without season data |
| seasonManager | get_challenge | deferred-no-handler | no handler | No active challenge widgets |
| seasonManager | get_max_active_challenges | deferred-no-handler | no handler | No active challenge widgets |
| seasonManager | challenge_is_dormant | deferred-no-handler | no handler | No challenge progress |
| seasonManager | get_challenge_expiration_date | deferred-no-handler | no handler | No challenge timer |
| seasonManager | get_season_end_time | deferred-no-handler | no handler | Season Agency content is not visible |
| seasonManager | get_loot_table_by_typeID | deferred-no-handler | no handler | No next seasonal goal |
| seasonManager | fill_up_and_return_challenges_for_character | deferred-no-handler | no handler | No active challenge recovery |
| seasonManager | claim_challenge_rewards | deferred-no-handler | no handler | No challenge claim controls |
| seasonManager | claim_goal_reward | deferred-no-handler | no handler | No goal reward icon |
| seasonManager | process_client_event | deferred-no-handler | no handler | Event processing requires an active season |
| seasonManager | reload_fsd_data | fixed | `crates/server/src/services/fixed.rs` | Refusal; no data reload or notification |
| battlepassSeasonMgr | CallSeasonDataRequested | fixed | `crates/server/src/services/fixed.rs` | None acknowledgement only |
| battlepassSeasonMgr | CallCompleted | deferred-no-handler | no handler | No loaded quest interface |
| battlepassSeasonMgr | CallRewardClaimed | deferred-no-handler | no handler | No loaded season reward interface |
| battlepassSeasonMgr | CallExtraGachaRewardClaimed | deferred-no-handler | no handler | No loaded extra reward interface |
| battlepassSeasonMgr | CallQAFinishMission | deferred-no-handler | no handler | Its QA control requires the loaded progress widget |
| milestoneMgr | ProcessCharacterLogon | fixed | `crates/server/src/services/fixed.rs` | None; no timer notification |
| milestoneMgr | ClaimRewards | deferred-no-handler | no handler | No milestone timer or claim widget |
| rewardMgr | GetDelayedRewardsByGroupIDs | fixed | `crates/server/src/services/fixed.rs` | Empty dictionary for requested groups |
| bluebirdIndustryMgr | RedeemTicket | fixed | `crates/server/src/services/fixed.rs` | Refusal; no item consumed or reward granted |
| activityMgr | GetActivityBadgeStatuses | fixed | `crates/server/src/services/fixed.rs` | Empty list |
| activityMgr | ActivitySeen | fixed | `crates/server/src/services/fixed.rs` | None; does not persist a seen state |

The battlepass request acknowledgement does not deliver an empty protobuf and does not set the loaded flag; no season data notification is sent. Activity popup preferences use their existing independent implementation.

## Paid and external platform boundaries

These boundaries do not implement paid services. The inactive NES state is neither completed nor skipped. The client gates cloud and monetization requests using missing configuration keys; local overrides are unsupported. Raffle QA and the inventory ticket menu remain reachable and are refused. No offers, rewards, cloud notifications or payment backend are supplied. The legacy `cache.Rowset` boundary is listed in the platform tooling table above.

| service | method | status | handler | answer or boundary |
|---|---|---|---|---|
| nes_intro | get_nes_intro_state | out-of-scope | `crates/server/src/services/fixed.rs` | Fixed compatibility: native UNSET (0), inactive |
| nes_intro | skip_nes_intro | out-of-scope | no handler | UNSET closes the remote skip gate |
| contextualOfferMgr | TrackOfferDelivered | out-of-scope | no handler | External offer telemetry; unhandled |
| contextualOfferMgr | TrackOfferSeen | out-of-scope | no handler | External offer telemetry; unhandled |
| contextualOfferMgr | TrackOfferBuyPress | out-of-scope | no handler | External offer telemetry; unhandled |
| contextualOfferMgr | TrackOfferClosed | out-of-scope | no handler | External offer telemetry; unhandled |
| contextualOfferMgr | TrackOfferPurchased | out-of-scope | no handler | External offer telemetry; unhandled |
| contextualOfferMgr | TrackOfferPurchasedSeen | out-of-scope | no handler | External offer telemetry; unhandled |
| raffleMgr | QA_SeedRaffles | out-of-scope | `crates/server/src/services/fixed.rs` | Fixed compatibility refusal; no test generation |
| monetizationMgr | CallOfferRequested | out-of-scope | no handler | Missing recommendation gate; unhandled |
| monetizationMgr | CallOfferClicked | out-of-scope | no handler | External banner click; unhandled |
| monetizationMgr | CallRequested | out-of-scope | no handler | Missing activity-type gates; unhandled |
| monetizationMgr | CallActivityEntryRewardClaimed | out-of-scope | no handler | No activity content or reward interface |
| gachaMgr | InjectTickets | out-of-scope | `crates/server/src/services/fixed.rs` | Fixed compatibility refusal; no item consumed or delayed success |
| cloudComputingMgr | CallCloudComputingOpened | out-of-scope | no handler | Missing cloud gate; unhandled |
| cloudComputingMgr | CallAddShipClicked | out-of-scope | no handler | Missing cloud gate; unhandled |
| cloudComputingMgr | CallRemoveShipClicked | out-of-scope | no handler | Missing cloud gate; unhandled |
| cloudComputingMgr | CallRemoveAllShipsClicked | out-of-scope | no handler | Missing cloud gate; unhandled |
| monetizationMgr | CallUserRequested | out-of-scope | `crates/server/src/services/fixed.rs` | Existing fixed compatibility ACK only; no user data response or notification |

## Corporations, alliances and owner lookup

| service | method | status | handler file |
|---|---|---|---|
| corpRegistry | InsertApplication | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | InsertInvitation | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMyApplications | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetApplications | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMyOldApplications | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetOldApplications | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateApplicationOffer | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetCorpWelcomeMail | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | SetCorpWelcomeMail | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateCorporationAbilities | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | AddCorporation | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetEveOwners | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMember | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMembersPaged | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMembersByIds | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMemberIDsByQuery | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMemberTrackingInfo | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetMemberTrackingInfoSimple | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateMember | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateMembers | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | ExecuteActions | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetTitles | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateTitle | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateTitles | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | DeleteTitle | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | CanLeaveCurrentCorporation | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | CanBeKickedOut | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | KickOutMember | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | KickOutMembers | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetPendingAutoKicks | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetNumberOfPotentialCEOs | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | ResignFromCEO | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | GetInfoWindowDataForChar | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateCorporation | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateDivisionNames | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | UpdateLogo | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | SetAccountKey | handled | `crates/server/src/services/corps/corp_registry.rs` |
| corpRegistry | CreateAlliance | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | GetSuggestedAllianceShortNames | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | ApplyToJoinAlliance | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | GetAllianceApplications | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | DeleteAllianceApplication | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpmgr | AuditMember | handled | `crates/server/src/services/universe/corpmgr.rs` |
| allianceRegistry | GetApplications | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | UpdateApplication | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | GMForceAcceptApplication | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | GetAlliance | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | GetAlliancePublicInfo | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | GetRankedAlliances | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | GetAllianceMembers | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | GetEmploymentRecord | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| allianceRegistry | GetMembers | handled | `crates/server/src/services/alliances/alliance_registry.rs` |
| lookupSvc | LookupEvePlayerCharacters | handled | `crates/server/src/services/lookup_svc.rs` |
| lookupSvc | LookupOwners | handled | `crates/server/src/services/lookup_svc.rs` |
| lookupSvc | LookupPCOwners | handled | `crates/server/src/services/lookup_svc.rs` |
| lookupSvc | LookupWarableCorporationsOrAlliances | handled | `crates/server/src/services/lookup_svc.rs` |
| lookupSvc | LookupNoneNPCAccountOwners | fixed | `crates/server/src/services/fixed.rs` |
| lookupSvc | LookupKnownLocationsByGroup | fixed | `crates/server/src/services/fixed.rs` |
| lookupSvc | LookupLocationsByGroup | fixed | `crates/server/src/services/fixed.rs` |
| lookupSvc | LookupCharacters | fixed | `crates/server/src/services/fixed.rs` |
| lookupSvc | LookupCorporations | fixed | `crates/server/src/services/fixed.rs` |
| lookupSvc | LookupFactions | fixed | `crates/server/src/services/fixed.rs` |

## Search-word moderation

| service | method | status | handler file |
|---|---|---|---|
| bannedwords | check_search_words_allowed | handled | `crates/server/src/services/bannedwords.rs` |

Search-word checks use the captured retail moderation rules. Missing rules and unsupported or failing regular expressions refuse the request rather than bypassing moderation. Resource Wars channel creation has no handler: its instance gate is closed and the channel lifecycle is not modeled.

## Ship SKINs

| service | method | status | handler file |
|---|---|---|---|
| shipCosmeticsMgr | ApplySkinToShip | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | GiveSkin | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | GMExpireSkinLicense | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | RemoveSkin | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | ActivateSkinLicense | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | ActivateStructureSkinR | out-of-scope | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | GetFirstPartySkinData | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | GetLicencedSkins | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |
| shipCosmeticsMgr | GetLicencedSkinsForShipType | handled | `crates/server/src/services/ship_cosmetics_mgr.rs` |

The handled calls cover ordinary first-party non-single-use licenses and compatible ships, subject to ownership, reachability, regional and role checks where applicable. Structure activation is deferred and explicitly refused without consumption, not declared permanently unsupported. GML administration is current-character only.

| gateway payload | kind | status | handler file |
|---|---|---|---|
| `eve_public.cosmetic.ship.api.GetRequest` | request | handled | `crates/server/src/gateway/ship_cosmetics.rs` |
| `eve_public.cosmetic.ship.api.GetAllInBubbleRequest` | request | handled | `crates/server/src/gateway/ship_cosmetics.rs` |
| `eve_public.cosmetic.ship.api.SetNotice` | notice | handled | `crates/server/src/gateway/ship_cosmetics.rs` |
| `eve_public.cosmetic.ship.api.SetAllInBubbleNotice` | notice | handled | `crates/server/src/gateway/ship_cosmetics.rs` |

Get returns 200 with explicit `no_skin` for a valid unskinned ship, 404 with no payload for an absent or invisible ship, and 500 for a missing actor or location. GetAll returns 200 with an empty `states` list for an empty valid scope. SetAll carries the repeated field `state`, not GetAll's `states`. Snapshots and post-commit updates use the same visibility facts as reads, including cloak visibility.

## Personal skill plans

| Gateway request | Handler |
|---|---|
| `eve_public.character.skill.plan.CreateRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.GetAllRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.GetRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.GetSharedRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.GetActiveRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.SetActiveRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.DeleteRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.SetNameRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.SetDescriptionRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.SetSkillRequirementsRequest` | `gateway/skill_plans/personal.rs` |
| `eve_public.character.skill.plan.milestone.GetAllRequest` | `gateway/skill_plans/personal_milestones.rs` |
| `eve_public.character.skill.plan.milestone.CreateRequest` | `gateway/skill_plans/personal_milestones.rs` |
| `eve_public.character.skill.plan.milestone.DeleteRequest` | `gateway/skill_plans/personal_milestones.rs` |
| `eve_public.character.skill.plan.milestone.SetDescriptionRequest` | `gateway/skill_plans/personal_milestones.rs` |

Shared plan lookup returns an absent plan when the definition is missing. The active response always contains a UUID, including sixteen zero bytes when tracking is cancelled. Certified tracking uses a static numeric plan ID encoded as a UUID. Milestone `SetDescription` is supported by the protocol; the current client has no known UI sender.

## Gateway transport

| service | method | status | handler file |
|---|---|---|---|
| eve_public.gateway.Requests | Send | handled | `crates/server/src/gateway/transport.rs` |
| eve_public.gateway.Notices | Consume | handled | `crates/server/src/gateway/transport.rs` |
| eve_public.gateway.Events | Publish | handled | `crates/server/src/gateway/transport.rs` |
| eve_public.gateway.Requests | Ping | handled | `crates/server/src/gateway/transport.rs` |
| eve_public.gateway.Notices | Ping | handled | `crates/server/src/gateway/transport.rs` |
| eve_public.gateway.Events | Ping | handled | `crates/server/src/gateway/transport.rs` |

Gateway transport uses HTTP/2 gRPC; request routing is in `crates/server/src/gateway/requests.rs`, and event handling is in `crates/server/src/gateway/events.rs`. Unclaimed client events are ignored with debug logging; client completion messages do not grant progress or rewards. Notices use the native target oneof for application, user, character, corporation, alliance or solar system, matched against current identity; application identifiers are 16-byte UUIDs. Replaced, closed or slow stream generations are cleaned up without unregistering a replacement. Delivery is best-effort after commit, with no durable replay; reconnect and Get recover committed state. Ownership lookup is not token authentication, so the gateway needs a trusted network or authentication front door.
