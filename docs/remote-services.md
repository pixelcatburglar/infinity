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

NPC station and solar-system data comes from static data; alliance sovereignty is not supported.

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

A disconnected member's fleet seat is kept for `fleets.reconnect_timeout_secs` (120 seconds by default, at most 120) so the client can rejoin automatically; seats do not survive a server restart.

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

## Player structure directory

| service | method | status | handler file |
|---|---|---|---|
| structureDirectory | GetMyCharacterStructures | fixed | `crates/server/src/services/fixed.rs` |

The character's player-structure directory is empty because player structures are not supported. The market's multi-buy window lists only NPC stations.
