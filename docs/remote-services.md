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
