# Effects

## Architecture and activation

The single registry in `crates/server/src/services/effects/registry.rs` resolves an
SDE effect ID to a behaviour kind and a descriptor carrying the effect's SDE
metadata, including its GUID, category, timing and range attribute IDs, and
behaviour flags. Explicit behaviour classifications and exclusions take
precedence over the generic modifier fallback. Effect names may be used to find
SDE data, but do not select runtime behaviour.

Activation checks that the item belongs to the pilot, is fitted to the active
ship, and is online where required. The shared validation applies at the
activation trust boundary; the online RPC and activation of the online effect
use one online operation. Scope is checked against the item's type and group:
explicitly excluded groups are denied, rather than admitting only a client-side
allowlist. A recognised kind is not necessarily an implemented behaviour.
Unsupported or out-of-scope activations are visibly refused, including those
with modifier data; they are never treated as a generic animation.

Unsupported attempts are logged and recorded once per effect name and type ID
per process. A GM with the existing slash role can use `/effects unsupported`
to list recorded pairs. The process-local list resets on restart and is not an
inventory of every unsupported SDE effect.

## Fitting and online resources

The shared fitting policy validates inventory moves and online transitions with
the same Fit evaluator. Fitting checks structural constraints, including rack
and slot availability, hardpoints, calibration, hull restrictions, fitted
group and type limits, and skill prerequisites. CPU and power shortages do not
prevent an offline fit: they refuse the online transition, using effective
output and load with the prospective module online. Numeric client-side fitting
warnings without corresponding UserErrors use the server's existing
`CustomNotify` fallback.

The online mutation core owns the database transition, weapon-bank changes and
the outbox for effect and attribute notifications. Legal ship priming makes no
online-state changes. Priming and boarding reconcile legacy overfits; GM dogma
overrides and skill losses also reconcile affected hulls. The cascade compares
the CPU and power load/output ratios, chooses the worse resource, and offlines
its largest contributing ordinary module before service modules, breaking
remaining ties by item ID. It reloads the effective fit after each offline so
lost online bonuses can cause further changes. Notifications and bank stops
are dispatched only after commit.

## Behaviour status

| Kind or family | Status |
|---|---|
| Turret, missile, local shield/armour/hull repair, propulsion, mining | Existing behaviour uses the shared lifecycle. |
| Capacitor boosters, neutralizers, nosferatu, capacitor transfer and fueled local repair | Supported through the shared lifecycle. Charged ancillary shield boosters bypass capacitor discharge without an amount multiplier; fueled ancillary armor repair pays normal capacitor and uses its fitted charge multiplier. Uncharged ancillary repairs use their normal capacitor cost and base amount. |
| Modifier-only and online effects | Supported through the modifier and online paths, subject to validation and scope. |
| Hacking and probe launch | Classified for existing exploration paths; classification alone does not guarantee every item or call path. |
| Projected web, warp disruption and scrambling, target painting, sensor damping, tracking and guidance disruption, remote sensor and tracking enhancement | Runtime projections use the shared Fit evaluator and actor-owned lifetimes. Warp scramble status refuses warp entry; a scram that blocks propulsion stops a running microwarpdrive and refuses a new activation, without stopping afterburners. |
| Remote shield, armor and hull repair | Locked ships and NPCs use fitted amounts and range/falloff, rechecked at application. Server policy applies shield at cycle start and armor and hull at cycle end. Ancillary variants use the same charge, reload and capacitor rules as local ancillary repairs. Mutadaptive remote armor repair is refused. No Crimewatch assistance flags are applied. |
| ECM | A known later family, visibly refused until implemented. |
| NPC remote shield, armor and hull repair | NPCs assist the most damaged eligible ally in their spawn group or ownership set, using fitted SDE repair amounts. |
| Smartbomb, Vorton, cloak, micro jump drive, salvage, tractor, scans, target assistance and bombs | Known later families, visibly refused until implemented. |
| Explicitly excluded capital, industrial, structure and other out-of-scope groups | Refused even when an effect has modifiers. |

## Module map

All paths below are under `crates/server/src/`.

- `services/effects/registry.rs`: numeric classification, scope and resolved
  effect metadata; `mod.rs` defines the kinds.
- `services/effects/lifecycle.rs`: activation, cycles, repeat and stopping;
  `weapons.rs`, `repair.rs`, `capacitor.rs`, `propulsion.rs`, `mining.rs` and
  `hacking.rs` contain the family-specific work; `projected.rs` constructs
  projected effects.
- `services/effects/environment.rs` and `notify.rs`: effect environment and
  notification payloads; `unsupported.rs` owns refusal diagnostics.
- `services/combat/` retains targeting, ammunition, health, damage and NPC
  state. `services/fit.rs` evaluates attributes. Space actor timers schedule
  cycle work.
- Capacitor transfer does not yet propagate Crimewatch assistance; neutralizers
  and nosferatu use the existing offensive aggression path. Reloads use the
  existing ammunition reservation and actor deadline mechanism. When an ancillary
  repairer stops with an empty magazine, compatible cargo charges are reserved
  for automatic reload; a running module does not reload automatically.
- Charge-sensitive cycles check pending reloads and guard consumption against the
  cycle-start fitted charge type. If a reload wins after capacitor discharge,
  the refused cycle refunds that discharge through the normal capacitor injection
  path. Recharge, intervening capacitor operations and the capacity clamp can
  change the final level before the refund; this is not an atomic reservation.
- `sde/dogma.rs` loads dogma data; `sde/dogma/python_effects.rs` supplements
  Python-only modifiers.

## NPC capacitor assistance

NPC capacitor transfers select another live NPC brain in the same system. If
the source belongs to a spawn group, the recipient must share one of its groups;
otherwise it must have the same ball owner. The hostile player target is never
an assistance recipient. Among recipients with a non-full capacitor and a
positive optimal/falloff range multiplier at sampled surface distance, the
lowest recharged capacitor fraction wins, with item ID breaking ties. No
eligible recipient means no transfer. The existing NPC effect timer drives the
attempt; the commit rechecks the hostile target generation, recipient
eligibility and range. NPC assistance does not generate Crimewatch flags.

## Wire contract

`OnGodmaShipEffect` carries eleven arguments: `(itemID, effectID, time, start,
active, environment, startTime, duration, repeat, error, actualStopTime)`.
The environment is the eight-element list `[itemID, charID, shipID, targetID,
otherID, [], effectID, structureID]`. A loaded charge or script is identified
by its client sublocation tuple `(shipID, flagID, typeID)` in `otherID`, not
by the database item ID; absent values are `None`.

`OnModuleAttributeChanges` contains entries `(OnModuleAttributeChange, owner,
item, attribute, time, new, old, None)`; a charge quantity change uses the
same sublocation tuple as its item. `OnSpecialFX` carries fourteen arguments:
`(shipID, moduleID, moduleTypeID, targetID, chargeTypeID, guid, isOffensive,
start, active, duration, repeat, startTime, graphicInfo, area)`.

`OnEwarStart` and `OnEwarEnd` each carry four arguments
`(sourceBallID, moduleID, targetBallID, jammingType)`. The last argument is a
client string slot, selected from effect chance flags and the effect-ID slot
table, not a numeric effect kind. A blocked warp returns `CustomNotify`.

## Fit evaluator data and scope

Fit evaluates character, ship, module, charge and skill nodes alongside active
brain items, drones and explicitly supplied incoming projections. Brain items
are limited to character-owned implants in flag 89 and boosters in flag 88;
hangar and cargo items do not apply. Player implant plugging and booster use
RPCs, including booster lifecycle, remain with skills-complete and are not
implemented here.
Character item info transmits base character attributes because the client
applies the compiled brain afterward. Effective character and training
attributes come from the same Fit evaluator, so implant bonuses are applied
once rather than doubled.

Modifier evaluation covers item, location, group and required-skill domains,
including character-location brain items and drones. Operation 8 applies
unpenalized percent modifiers; special operation 9 derives skill level from
skill points and rank. Overload modifier data is evaluated only when the
snapshot marks a node overloaded; this does not add an overload RPC.
Ordinary numeric evaluation retains per-fit memoization, not a persistent
keyed cache with invalidation. Client stacking penalties independently truncate
the strongest upward and downward modifiers to eight each.

`Fit::load_projected` is the single loading implementation. Each incoming
effect carries source-effective module operands and separate charge/script
operands, so equal attribute IDs on the two nodes do not collide. The target
evaluates only that effect's target modifiers; it does not load the source
ship. In space, `Fit::load_in_space` atomically snapshots incoming projections
and the target revision from the system actor; `Fit::load` remains for
non-space and character-only loads. Deferred fit and kinematic commits check
the revision, including at destiny application; warp entry retries when its
snapshot becomes stale rather than committing an obsolete permission.

Projections are transient, target-keyed system-actor state, indexed by source
module: each cycle replaces that module's contribution. Stop, target loss,
expiry and source or target departure remove the relevant contribution.
Finite cycles expire after their duration plus exactly one destiny tick of
grace; an indefinite duration has no expiry. Refreshes reevaluate Fit outside
the actor and queue kinematics through destiny. Runtime state is lost on restart.

A scram-blocked microwarpdrive is removed from the active Fit input immediately,
while its running cycle remains owned until the normal stop publishes the real
stop notification and performs its teardown. Ordinary deactivation continues
to affect Fit until the cycle ends.

The deterministic projection range multiplier uses sampled surface distance: strength
is 1 through inclusive optimal, then `0.5^(((distance - optimal) / falloff)^2)`.
Optimal is clamped to zero; non-positive falloff is a step to zero beyond
optimal, and non-finite inputs yield zero. This is strength scaling, not a hit
roll. Player and NPC projections share this path and the Fit modifier evaluator.
Generic NPC effect modules have distinct process-local identities and independent
actor timers; target generations prevent stale queued cycles from restoring a
lost projection. Drone gameplay and new drone RPCs are not supplied. No
real-client walkthrough is claimed by this documentation.

NPC shots snapshot incoming projections from the actor and evaluate their type
with the same Fit evaluator. Turret optimal, falloff and tracking, and missile
velocity, flight-time, explosion velocity and cloud-size multipliers are consumed
by NPC weapons. Missing turret range attributes retain the existing NPC
fallbacks. Missile multipliers use the type or dogma metadata default, falling
back to one if neither exists.

Sensor damping refreshes NPC scan resolution and dogma maximum target range
under the projection revision guard. The latter is enforced when the type
supplies it or dogma metadata supplies a positive default, independently of
the AI aggro range. Removing a
projection restores the base values. An existing target outside the refreshed
limit is lost; a pending lock retains its original deadline, while later attempts
use the refreshed scan resolution.

## Weapon accuracy and application

Turret accuracy follows native `Ballpark.cpp` `GetAccuracy`: with center
distance `d`, relative position `r`, relative velocity `v`, surface distance
`s = max(0, d - sourceRadius - targetRadius)`, and angular speed `ω` derived
from relative motion, the chance is `0.5^((ω * optimalSigRadius / (tracking * targetSig))² +
(max(0, s - optimal) / falloff)²)`. Identical balls, missing balls, negative
optimal and zero center distance yield no hit. Two deliberate NPC-compatible
deviations handle missing or zero stats: zero falloff is a step (zero
range term inside optimal, a miss outside), and zero tracking omits the
angular term. Target signature uses the positive cached ball value, then its
type's signature, then the SDE attribute default.

Missile application at impact uses target absolute speed and signature:
`sig_f = signature / max(aoeCloudSize, 1)`, and
`vel_f = (sig_f * aoeVelocity / speed)^aoeDamageReductionFactor`;
damage fraction is `min(1, sig_f, vel_f)`. Zero speed or zero explosion
velocity sets `vel_f` to one. The impact samples the target once.

GMs can use `/hitchance <targetID>` to inspect each fitted turret's item and
type IDs, center and surface distances, relative speed, angular speed and its
squared value, fitted tracking, optimal signature, target signature, optimal, falloff and resulting
chance. Values have at least 17 significant digits for hand evaluation of the
native formula. Both balls are sampled together, and the source Fit includes
incoming projections. This is a diagnostic, not a client UI comparison or a shot.

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
group and type limits. CPU and power-grid availability are checked when a module
is brought online, not merely when it is fitted.

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
