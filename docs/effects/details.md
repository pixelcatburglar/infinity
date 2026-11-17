# Effect details

## Damage and ship loss

Ordinary hits and repairs, including remote repair, repair drones and GM
commands, change shield charge, armor damage and hull damage in the system
actor's memory. Dirty persistent ships and drones are batch-checkpointed with
positions on the configured snapshot cycle (30 seconds by default), and at
docking, jump or transfer, boarding or ship swap, logoff and shutdown. Drone
condition is also saved on return to the bay. Shields recharge from the saved
timestamp. A crash can lose condition changes since the last successful save.
A ship with no condition attributes is at full health.

Each attacker's contribution records the damage actually dealt, the attacking
ship and weapon, and whether the aggression was illegal; once illegal, it stays
illegal. Activating a weapon without dealing damage gives no contribution. These
contributions live only in actor memory and are lost on restart; there is no
`game.combat_contributions` table. The attacker of the lethal hit gets the
final blow, and a missile or bomb keeps the ship and weapon it was launched
from. Killmails list the attackers within the configured window.

A lethal hit marks the ship as dying, preventing further hits while the loss is
settled. For a piloted ship, one transaction deletes the ship, its entire
active contents subtree at any depth (including items owned by other players),
the pending-reload reservations whose weapon or original source lies inside
that subtree, and the subtree's mutable attachments: fitted charges, weapon
banks, insurance contracts, blueprint state, attributes, configuration, dirt,
SKIN applications and dynamic metadata, without refunding consumed
reservations; it places the pilot in a capsule or at their home station,
records the killmail with the actual final blow, settles insurance and wallet
changes, and queues `ShipLost`, whether or not the pilot is connected.
Unpiloted ship loss uses the same whole-subtree deletion inside a transaction,
but creates no capsule, wreck, killmail, insurance payout or reward. A
killmail's victim item ID is unique. If settlement fails or its outcome is
uncertain, reconciliation checks the stored state: a confirmed rollback
restores the prior condition, while a committed loss is published or retired.
An inconclusive result remains pending for reconciliation.

This covers ordinary typed damage. Breacher damage over time and its defensive
module are not implemented.

## SDE and Python modifiers

At startup, dogma loads SDE effects and modifier rows. The Python modifier
table matches the actual SDE effect names at load time, for example
`remoteWebifierFalloff`, `remoteTargetPaintFalloff`, `remoteSensorDampFalloff`
and `remoteSensorBoostFalloff`. Its rows supplement an effect only when the SDE
modifier list is empty; they are never combined with a non-empty list. The
result follows the normal indexed SDE/Fit evaluation path. Modifier data for
a later family does not implement or enable that family's activation.

## Adding a family

1. Identify its SDE effect IDs, item groups, metadata and client behaviour. Add
   named IDs and classification in the registry; maintain explicit exclusions
   until the corresponding behaviour is actually supported. Do not dispatch by
   effect-name strings or use the generic modifier path to bypass a refusal.
2. Implement the behaviour under `services/effects/` and integrate it with the
   shared lifecycle, validation, resolved metadata and exhaustive kind matches.
   Read effective attributes through Fit, schedule space work on the actor, and
   use the shared notification builders.
3. Cover registry resolution, scope/refusal boundaries and observable cycle and
   wire behaviour in the existing separate test files. Update this document
   when support changes. Protocol and gameplay changes also need real-client
   verification.

## Fitting enforcement

`services/effects/fitting.rs` owns the shared fitting, onlining, activation and
resource-cascade policy. It reads effective values through `Fit` and its
modifier table rather than calculating a separate set of fitting attributes.
Structural fitting checks include hull and rack restrictions, slot occupancy,
skills, turret and launcher hardpoints, rig size and calibration, and fitted
group and type limits. CPU and power-grid availability are checked when a
module is brought online, not merely when it is fitted.

`services/effects/online.rs` owns the guarded online-state mutation. It queues
effect and attribute notifications and any weapon-bank changes in the outbox,
which dispatches after the transaction commits. Reconciliation reloads Fit
after each module is offlined. It chooses the resource with the worst load-to-output
ratio, choosing CPU on a tie; among online modules consuming that resource it
chooses ordinary modules before service modules, then highest resource usage,
then highest item ID. A legal load requires no state writes or notification
pushes.

The policy is invoked by fitting and unfitting, online changes, ship load,
boarding and character selection, GM fitting and dogma overrides, and skill loss.
Where the client presents a fitting warning only as a UI label rather than a
catalogue error, the server uses a localized `CustomNotify` fallback. This
description does not claim real-client verification.
