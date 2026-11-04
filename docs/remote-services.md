# Remote service coverage

This document lists client remote-service calls the server answers specially, grouped by feature: handled, fixed, retail, or out of scope. Rows are added as calls are dealt with. A call not listed is answered by its feature module or logged as unhandled.

## Status definitions

- **handled**: A real implementation handles the method.
- **fixed**: A constant compatibility answer is provided for an unmodelled feature; this does not mean the feature is fully supported.
- **retail**: A static retail capture provides the answer.
- **out-of-scope**: The feature is unavailable.

## Character selection

| service | method | status | handler file |
|---|---|---|---|
| charUnboundMgr | GetQAStarterSystemIDs | fixed | `crates/server/src/services/fixed.rs` |
| charUnboundMgr | GetCharacterInfo | fixed | `crates/server/src/services/fixed.rs` |

The QA starter-system override list is empty; normal starter-system allocation is unchanged. The legacy petition window offers no character choices before character selection. Petition categories, submission, and external support are not guaranteed.

## Station

| service | method | status | handler file |
|---|---|---|---|
| stationSvc | GetStationsForOwner | handled | `crates/server/src/services/station/station_svc.rs` |
| stationSvc | GetStation | handled | `crates/server/src/services/station/station_svc.rs` |
| stationSvc | GetSolarSystem | handled | `crates/server/src/services/station/station_svc.rs` |
| stationSvc | GetSystemsForAlliance | out-of-scope | — |
| station | GetGuests | handled | `crates/server/src/services/character/station.rs` |

NPC station and solar-system data comes from static data; alliance sovereignty is not supported.

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

## Mining ledger

| service | method | status | handler file |
|---|---|---|---|
| corpMiningLedger | GetObserversWithMiningEvents | fixed | `crates/server/src/services/fixed.rs` |
| corpMiningLedger | GetObserverLedger | fixed | `crates/server/src/services/fixed.rs` |

Corporate mining observers and structure ledger history are unsupported; their queries return empty lists. Personal mining history is unchanged.

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

## External phone binding

| service | method | status | handler file |
|---|---|---|---|
| mobileCollectMgr | MobileBindSucceed | fixed | `crates/server/src/services/fixed.rs` |

The callback receives a fixed `None` reply only to complete the call; it does not verify or persist a binding. Phone binding, binding state and rewards remain unsupported.

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
| skillHandler | PurchaseSkills | handled | `crates/server/src/services/skills/purchase.rs` |

The bound skill handler returns the bought type IDs in request order and sends the same committed skill updates as skillbook injection. Skills in the client's hidden skill groups cannot be bought.

## Expert systems

| service | method | status | handler file |
|---|---|---|---|
| expertSystemMgr | GetMyExpertSystems | handled | `crates/server/src/services/expert_systems.rs` |
| expertSystemMgr | ConsumeExpertSystem | handled | `crates/server/src/services/expert_systems.rs` |
| expertSystemMgr | RemoveMyExpertSystem | handled | `crates/server/src/services/expert_systems.rs` |

The installed expert systems are returned by type ID with their install and expiry times as FILETIME. Consuming takes an item ID and removing a type ID; both return `None` once the change is saved, and otherwise refuse with a message and change nothing. Every change sends `OnExpertSystemsUpdated` with the full set.

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
| structureDirectory | GetMyCharacterStructures | fixed | `crates/server/src/services/fixed.rs` |

The character's player-structure directory is empty because player structures are not supported. The market's multi-buy window lists only NPC stations.

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
| invbroker | GetDamageForCrystals | handled | `crates/server/src/services/inventory/inv_broker.rs` |
| invbroker | SetLabel | handled | `crates/server/src/services/inventory/inv_broker.rs` |
| invbroker | TrashItems | handled | `crates/server/src/services/inventory/inv_broker.rs` |
| invInventory | ALSCConfigGet | fixed | `crates/server/src/services/fixed.rs` |
| invInventory | ALSCConfigSet | out-of-scope | `crates/server/src/services/fixed.rs` |
| invInventory | GetPriceForItems | out-of-scope | `crates/server/src/services/fixed.rs` |

Audit log secure containers report that they are not configured; saving their configuration and `GetPriceForItems` are refused.

## Crates

| service | method | status | handler file |
|---|---|---|---|
| crateService | PeekInsideCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | PeekInsideFixedCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | CustomGetLootFromFixedCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | GetCrate | fixed | `crates/server/src/services/crates/crate_service.rs` |
| crateService | GetCrateLocation | fixed | `crates/server/src/services/crates/crate_service.rs` |
| crateService | OpenCrate | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | CustomOpenCrate | fixed | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimLootFromCrateStack | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimLoot | handled | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimSkillBundle | fixed | `crates/server/src/services/crates/crate_service.rs` |
| crateService | ClaimFitting | fixed | `crates/server/src/services/crates/crate_service.rs` |
| skillHandler | GetSkillBundleInfo | fixed | `crates/server/src/services/skills/skill_handler.rs` |
| ship | GetShipFittingInfo | fixed | `crates/server/src/services/station/ship.rs` |
| storeManager | can_purchase_offer | handled | `crates/server/src/services/store/store_manager.rs` |
| storeManager | is_offer_purchasable | handled | `crates/server/src/services/store/store_manager.rs` |

The three preview calls return the crate's contents as `(typeID, quantity, None)` entries and change nothing; a preview of a crate item needs the same ownership, corporation hangar role and reachability as moving it. `GetCrate` returns an empty list. `OpenCrate` opens one ordinary direct-list crate. `ClaimLootFromCrateStack` opens up to 30 from one stack, all or nothing, and returns `(opened, requested)`; a larger stack opens 30 per call. Rewards are created immediately in the opener's personal hangar at the current station when docked, or in the active ship's cargo in space; the operation is refused if the cargo lacks room. A corporation-owned crate requires hangar Take permission and its rewards go to the opener. Source consumption, reward creation and receipts commit together, followed by notifications.

`ClaimLoot(itemID, destinationID, destinationFlagID)` confirms the receipt recorded at opening only for the character who opened the crate and the exact destination. Repeats succeed, even after the reward is used or merged; the call never creates or moves items or sends notifications. There is no request-keyed replay protection for opening: client call IDs are fresh per call. `CustomOpenCrate`, `GetCrateLocation`, `ClaimSkillBundle` and `ClaimFitting`, as well as skill-bundle and fitting details, are refused with a message. The store checks answer no for an unknown offer, a daily gift already claimed today, or a crate.

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
| corpRegistry | CreateAlliance | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | GetSuggestedAllianceShortNames | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | ApplyToJoinAlliance | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | GetAllianceApplications | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
| corpRegistry | DeleteAllianceApplication | handled | `crates/server/src/services/corps/alliance_corp_registry.rs` |
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
