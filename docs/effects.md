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

Recurring work (cycle repeats, drone phases, shots) is meant to run from the
system actor's state without a database round trip. `AGENT.md` (Recurring work
in space) states the rule; `docs/limitations.md` lists the paths that still
load or write on every repetition. A resident loaded charge pays exactly one
signed 64-bit unit in the actor, marks that absolute dirty and publishes the
existing quantity notice. One finite permit, bound to the slot's incarnation
and type, admits only work that may still pay. The captured charge token
carries the membership and must name the same ship, module and type before
payment; success, refusal, cancellation, stop and drop release the permit
without waiting out a cycle timer or a missile already in flight. The same
actor turn removes an emptied charge node.
Pending means a reload reservation still owns the slot; unknown means the
durable outcome was not established. Either refuses a new payment, and unknown
stays unusable until a fresh read reseeds it.

Dirty loaded quantities join the position snapshot under its gate. One absolute
batch updates positive ship, flag and type rows and deletes zero rows; it never
inserts. Only a matched row whose identity and quantity still equal the capture
is cleared, so a shot during the write stays dirty. An unknown snapshot result
stays dirty for the next period. A charge-changing business or boundary
transaction first waits out admitted permits, then settles that absolute before
its own decision. Known commit publishes the new absolute and pending flag;
known rollback keeps the previous absolute and dirty bit; unknown invalidates
the quantity and reseeds the durable charge, pending reservation and module.
A failed fresh read stays held and unknown, and none of these paths replays
cargo creation, a refund or a probe launch. The default period is 30 seconds.
After three consecutive unfinished loaded periods, or during shutdown drain,
only new loaded use pauses. A crash or an older fresh value can drop unsaved
consumption. Not every effect avoids SQL, and loaded charges are not zero-loss.

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

Fitting operations publish one final effective-attribute batch per affected hull
after inventory changes. In space the system actor merges committed fitting
fields into its current fit and rebases current condition and capacitor without
discarding live changes. For stored hulls the transaction samples condition
before fitting and rebases it against the final fit; shield capacity or
recharge-rate changes checkpoint the rebased condition in the same transaction.
Capacity changes precede their current charge or damage values in the batch.
Recharge-rate changes also publish the sampled current charge. Fitted motion is
evaluated from the current actor fit and queued for the next destiny boundary;
membership, live phase and projection revision fence the queued action.

Committed writes outside inventory changes publish precise field deltas
instead of reloading a fit into the actor. A GM dogma override on a hull or a
fitted module captures its before-fit inside the transaction before the write,
so the published delta is exactly the committed change; a real owner's drone
receives only its own override on its live instance. Overheat damage short of
burnout publishes the exact persisted damage values as one field delta, while
a burnout still publishes the single final fitting measured before any damage
write. The real owner, never a delegated controller, supplies the values and
receives the attribute notifications.

In space, a module's real active-flag transition publishes the changed
effective attributes to the piloted ship's owner on
`OnModuleAttributeChanges`. The resident fit is compared before and after
the flag change over the same candidate set the fitting publisher uses:
ship values key to the ship, module values to their own items,
capacity rows carry their current charge or damage pair, and a
recharge-only change republishes the stored charge. A normal activation
or cycle stop sends `OnGodmaShipEffect` before the attribute batch; an
immediate forced stop may publish the batch before a later stop effect or
none at all. A repeated same-state write publishes nothing, and a ship
without a reachable pilot skips only the publication while the flag
change, drone propagation and output rebase still run. Each emitted row
also records a DEBUG event carrying the source module, affected item,
attribute and both values for deployed verification.

In space the same before/final emitter publishes the piloted ship's
effective attributes on `OnModuleAttributeChanges`, keyed to the ship and
its fitted modules, when other actor mutations change them: projection
apply, operand change, identity replacement, removal and expiry (a source
module moved to a new target publishes on each); character input
replacement and deadline expiry of skills, implants, boosters and expert
systems; and overload on and off. Old values are captured before any
expiry runs, the mutation applies once, and a combined expiry and mutation
emits one batch rather than two, pairing capacity rows with their rebased
charge and republishing the stored charge on recharge-only changes. A
renewal or same-state write that changes no value sends nothing, and a
ship without a reachable pilot updates actor outputs without building
notification rows. Existing effect, heat, EWAR and brain notifications
keep their order and payloads. Each resident owned drone publishes
through the same emitter and before/final receipt to its real owner,
never a delegated controller: rows key to the drone's own `Key::Drone`
item and pair its own condition, charge and capacitor values. One
receipt per drone covers owner-ship input propagation, direct character
inputs and the ship publication's owned-drone fanout, cold
owner-package handoff, direct drone projections and the copied
owner-ship projection domain, and real deadline expiry, keeping the
call-local dedup, capacity pairing and same-value silence. A ship batch
never carries a resident drone's rows, and a drone batch never carries
copied owner-ship keys. Abandoning a drone publishes its old-to-final
own attributes to the former owner in the actor turn before the state
and activity notifications that make the client unload it; bay and
stored priming stay on the existing cold paths. A drone without a
reachable owner, and NPC or type-only records, update actor outputs
without publishing.

Skill, implant, booster and expert-system changes publish the complete
character domain through the same committed outbox to every live recipient the
pre-transaction fences captured: a piloted hull and that owner's drones in any
system. The publish happens even with no online or fitted module at all; an
actor record that no longer matches owner, generation or live phase is skipped
rather than reloaded. Real booster and expert-system deadlines are then expired
by the actor's own deadline gate, not by republishing or a TTL reload.

Charge completion counts the charge currently loaded under the existing
completion lock plus only reservations that still exist. A magazine emptied
while waiting is a cold insertion: it primes the complete sublocation and
publishes fitted topology. A positive same-type refill publishes only a
quantity delta from that current amount. A different charge type, or a module
that is no longer the original module at the same ship and fitting slot for
the reservation owner, rolls the whole completion back and keeps the
reservations. Actual cold charge insertions and removals use the same
before/final fitting boundary, without online-resource reconciliation. A type
switch that completes removes the old key before the new key is primed;
unloading publishes its zero quantity. Shot and ordinary
quantity consumption do not load or publish a new fit. Client verification of
these charge notifications and modifier transitions remains outstanding.

A turret whose type declares no charge group or size fires without a loaded
charge: activation skips the loaded-charge guard, each cycle reads raw damage
from the module's own effective damage attributes times its effective
multiplier, and no charge transaction, quantity change or reload runs. The
activation carries no charge sublocation. Charged turrets, launchers and every
other weapon family keep the existing admission, consumption, reload and bank
behaviour; modules whose active effect is not a turret are unaffected.

## Behaviour status

| Kind or family | Status |
|---|---|
| Turret, missile, local shield/armour/hull repair, propulsion, mining | Existing behaviour uses the shared lifecycle. |
| Capacitor boosters, neutralizers, nosferatu, capacitor transfer and fueled local repair | Supported through the shared lifecycle. Charged ancillary shield boosters bypass capacitor discharge without an amount multiplier; fueled ancillary armor repair pays normal capacitor and uses its fitted charge multiplier. Uncharged ancillary repairs use their normal capacitor cost and base amount. |
| Modifier-only and online effects | Supported through the modifier and online paths, subject to validation and scope. |
| Hacking and probe launch | Classified for existing exploration paths; classification alone does not guarantee every item or call path. |
| Projected web, warp disruption and scrambling, target painting, sensor damping, tracking and guidance disruption, remote sensor and tracking enhancement | Runtime projections use the shared Fit evaluator and actor-owned lifetimes. Warp scramble status refuses warp entry; a scram that blocks propulsion stops a running microwarpdrive and refuses a new activation, without stopping afterburners. |
| Remote shield, armor and hull repair | Locked ships and NPCs use fitted amounts and range/falloff, rechecked at application. Server policy applies shield at cycle start and armor and hull at cycle end. Ancillary variants use the same charge, reload and capacitor rules as local ancillary repairs. Mutadaptive remote armor repair is refused. No Crimewatch assistance flags are applied. |
| ECM and burst jammers | Targeted and area jamming use the shared lifecycle, fitted sensor strengths and actor-owned jam lifetimes. |
| NPC remote shield, armor and hull repair | NPCs assist the most damaged eligible ally in their spawn group or ownership set, using fitted SDE repair amounts. |
| Smartbombs, bombs and Vorton weapons | Area pulses, delayed bomb impacts and chained Vorton hits use the shared lifecycle and damage paths. Vorton hits apply the shared missile application per target; no tracking or hit roll is involved. In-range missiles and bombs also take the pulse's base damage after source admission, with no friendly filter; a destroyed bomb does not detonate. |
| Cloak, micro jump drive and compression | Actor-owned cloak visibility, queued micro jump movement and fitted compression availability use the shared lifecycle. Micro jump range uses fitted dogma when the module type has a range row; otherwise it uses the client 100 km distance. Invalid fitted ranges are rejected. |
| Salvage, tractor, cargo, ship and survey scanners, automatic and passive targeting | Supported through their shared lifecycle families. Survey results come from the separately triggered modern `miningScanMgr.perform_scan` RPC, not a pushed module result. |
| Precursor disintegrator spool | Supported for fitted players, banks and NPCs through the shared lifecycle and one pure ramp: base at bind, fitted per-cycle step, fitted cap, reset on a replaced incarnation. A bound NPC shot that cannot fire, target loss and a damage-free resident fit each end the ramp and its beam. |
| Burst jamming and probe launch | Visibly refused by this lifecycle. |
| Explicitly excluded capital, industrial, structure and other out-of-scope groups | Refused even when an effect has modifiers. |

## Module map

All paths below are under `crates/server/src/`.

- `services/effects/registry.rs`: numeric classification, scope and resolved
  effect metadata; `mod.rs` defines the kinds.
- `services/effects/lifecycle.rs` owns activation, cycles, repeat and stopping;
  `behaviors.rs` dispatches family validation, cycle work and cleanup. The
  family modules include `weapons.rs`, `repair.rs`, `capacitor.rs`,
  `propulsion.rs`, `mining.rs`, `ecm.rs`, `projected.rs`, `aoe.rs`, `bombs.rs`,
  `vorton.rs`, `cloak.rs`, `mjd.rs`, `compression.rs`, `tractor.rs`,
  `scan.rs`, `targeting.rs` and `salvage.rs`; `hacking.rs` serves hacking.
- `services/effects/environment.rs` and `notify.rs`: effect environment and
  notification payloads; the shared `OnSpecialFX` encoder lives in
  `space/mod.rs`. `unsupported.rs` owns refusal diagnostics.
- `services/combat/` retains targeting, ammunition, health, damage and NPC
  state. `services/fit.rs` evaluates attributes. Space actor timers schedule
  cycle work.

- `services/effects/spool.rs` owns the pure ramp arithmetic, binding identity
  and the client's `graphicInfo` payload; `space/npc/spool.rs` keeps the NPC
  brain's bound ramp and sustained beam, and `space/combat/spool.rs` keeps the
  presentation-only HUD readouts on the actor's combat record.
- `space/asteroids.rs`: the solar-system actor owns asteroid identity, rock
  quantities, field batches and regrowth timers. `services/mining.rs` keeps
  yield settlement, specialized hold routing, inventory notifications and
  ledger facts; mining validation checks rock identity and sampled range in one
  actor query.
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

Salvage yield tables do not ship with the client: the server substitutes a
configurable random selection from published Salvaged Materials types, and
preserves loose wreck contents in a positioned cargo container. The committed
salvage outcome is authoritative; live container publication is best effort and
failures are warned, with no automatic recovery. Tractor beams follow to surface
contact and restore the target's movement properties on stop.
Bomb and micro-jump heading follows moving velocity; when stationary it uses
the last GOTO direction, then the existing negative Z direction.

## Overload and heat

Overload uses the existing Fit category-5 modifier gate, not a separate bonus
evaluator. A module must have an overload effect, be online and unburnt, meet
its Thermodynamics requirement, and have an implemented underlying behaviour;
unsupported families remain refused. Single-module `Overload` and
`StopOverload` return `None` remotely. `OverloadRack` and `StopOverloadRack`
return lists of affected module IDs, even though the local rack-stop wrapper
returns `None`. Rack operations include weapon-bank members and validate the
set before changing it.

Starting overload on a running module takes effect at its next cycle start;
stopping takes effect at the current cycle end, which still rolls overloaded
damage. Starting an idle module primes it immediately, without a heat source,
so its first cycle is overloaded. Each running overloaded bank member is an
independent source. Effect and heat-source notifications occur at effective
boundaries: `OnHeatAdded` and `OnHeatRemoved` connect and disconnect sources,
not merely requested changes.

The system actor advances each rack's heat curve on its existing tick, settling
the old curve before source changes and at cycle ends. Connected sources add
their fitted heat absorption rates; capacity, generation and dissipation also
come from Fit. With incoming heat the curve approaches capacity exponentially;
without it heat decays exponentially and is rounded to zero at the cooling
cutoff. The HUD reports heat divided by capacity.

For each completed overloaded source cycle, the server uses the published
[EVE University master overheat damage formula](https://wiki.eveuniversity.org/Overheating)
as its damage policy, not as a client-provided formula. It rolls once for
each online recipient in the same rack:

`chance = clamp(heat / capacity, 0, 1) * (online high + medium + low modules) / (high + medium + low + rig slots) * attenuation^distance`

Slot counts and attenuation are fitted values. Distance is the absolute
physical slot difference without wrapping; empty and offline slots still count
in that distance, but offline modules cannot receive damage. A successful roll
adds the source's fitted `heatDamage`, capped at the recipient's fitted hit
points. Fit already applies Thermodynamics to `heatDamage`; no separate skill
multiplier is applied.

Module damage uses the existing item attribute 3 and is persisted transactionally
without a schema change. Burnout offlines the module through the existing online
and weapon-bank path; a burnt bank member stops the bank, without burning its
healthy members. Notifications of persisted changes follow commit. Station
repair and the GM space heal handle this damage, without bringing a burnt module
online. Space heal commits fitted-module damage before synchronously enqueueing
the guarded actor update under the market lock; an invalidated target reports a
module-only result. A retry patches all fitted modules even if stored damage is
already zero. The actor clears rack heat without changing overload membership,
pending boundaries or subsequent cycles, which may generate heat again.
Old or delayed heat damage samples covered by the reset are discarded without
stopping an ongoing cycle; an activation that is genuinely invalid still fails.

Rack heat, priming and overload are runtime-only: they reset on docking, jump,
logoff, leaving the ship or server restart, but persist through warp. Reset on
docking is server policy, unlike the public wiki's docked cooling behaviour.
NPC overload, paste repair, tether repair and structure repair are unsupported.

## Drones

Drones share the sequential NPC/drone worker, effect registry and Fit evaluator.
Their persistent inventory item ID is also their space ball ID; drones have no
pilot character ID. Position and damage are persisted lazily. Orders, control
and pending mining yield exist only at runtime; reconnecting to drones is not
supported. An owner may order an uncontrolled owned drone to ReturnBay: its
truthful owned state suppresses the client's manual Scoop option. Abandoned or
stateless drones can be manually scooped.

Mining and salvage drones have no mass or agility in the static data; the server gives them the common light-drone values (3000 kg, agility 100).

Published category 18 drones have no maxTargetRange or scanResolution rows,
and both attributes default to zero. Zero max target range adds no drone-local
lock limit; zero scan resolution locks immediately in action range. Owner
control range and action range remain enforced, and positive values retain
their normal limits and shared lock timing.

Drone launch preferences are synchronized by the client before a menu launch
(`ui/services/menusvc.py:3613-3615`, `environment/godma.py:2342-2363`);
individual preference changes also send patches, with initial values from SDE
attribute defaults (`ui/inflight/drones/droneSettings.py:8-25`). The server
keeps the validated aggressive and focus-fire preferences per character only in
memory, preserves untouched fields on partial changes, and clears them on logout.
Until synchronization, drones use passive, unfocused behavior. No preference
persistence or real-client verification is claimed.

Aggressive idle controlled drones react only to accepted incoming attacks against
their owner's piloted ship, using the same aggressor roster as Guard. Current
live visibility, fitted control and action range, lock, jam and existing legal
retaliation gates still apply. With focus fire, all idle drones select the lowest
eligible aggressor ID; without it, sorted idle drone IDs are distributed
round-robin over sorted eligible aggressor IDs. Explicit work, return and
delegated orders are not retargeted. This deterministic selection is server
policy: the client names the checkbox and hint but specifies no algorithm.
An Assist or Guard cycle clears its delegated order when the delegate leaves the owner's fleet, is absent, warping or cloaked, before combat source admission and on revalidation before applying the cycle.

Mining drones physically carry pending intent to the ship; asteroid extraction
and hold delivery commit together at contact. A full hold changes neither.
Drone deaths do not grant NPC rewards. Salvage drone commands are unsupported,
and remote assistance does not generate Crimewatch assistance flags.

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

## Electronic jamming

Targeted ECM requires a live locked target, neither ship in warp, and positive
range strength. The victim's strongest positive effective racial sensor selects
the matching racial jammer strength. Jam probability is that strength times the
shared range multiplier divided by the effective sensor strength, capped at one;
invalid or non-positive strengths cannot jam. ECCM and other fitted sensor
modifiers increase the effective defense through Fit.

Burst jammers require no lock and ignore any supplied target. They attempt each
eligible ship in the burst radius except the source, recording aggression for
each player victim regardless of the roll. Successful jams clear existing and
pending locks and prevent new locks until expiry. Jam state is transient: each
source/module has its own expiry and refresh generation, and overlapping jams
keep the target jammed until the last live record expires. Stopping a module
does not cancel a successful jam's chosen duration. Runtime state is lost on
restart.

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
`(shipID, moduleID, moduleTypeID, targetID, otherTypeID, guid, isOffensive,
start, active, duration, repeat, startTime, timeFromStart, graphicInfo)`;
`otherTypeID` is the charge type or wormhole class, and `duration` is an Int
for space transitions and a Float for module and GM effects.

`OnEwarStart` and `OnEwarEnd` each carry four arguments
`(sourceBallID, moduleID, targetBallID, jammingType)`. The last argument is a
client string slot, selected from effect chance flags and the effect-ID slot
table, not a numeric effect kind. A blocked warp returns `CustomNotify`.

`OnJamStart` carries `(sourceBallID, moduleID, targetBallID, "electronic",
startTime, duration)`; `startTime` is FILETIME and `duration` is milliseconds.
`OnJamEnd` carries only the first four fields. Both identify an individual
source/module jam. The corresponding electronic `OnEwarStart` and `OnEwarEnd`
use the four-field EWAR contract above.

## Sustained effects and scene replay

A sustained effect is one the client keeps drawing while its cycle runs:
fueled local repair, remote repair, capacitor transfer, neutralizer,
nosferatu and capacitor booster animations, ECM, mining and cloud-mining
beams, salvage, the cargo, ship and survey scanner beams, tractor beams and
paid Entosis links. When an observer enters a scene, or a ship becomes
visible by uncloaking, every such effect that is still inside its checked
deadline and whose source and target that observer can see is replayed: the
wire event carries the original duration and original start time and reports
the remaining repeat count and the current cycle phase, rather than
claiming a fresh start or passing the total remaining time as a new
duration.

Transient effects are never recorded for replay: turret and missile shots
and launches, smartbomb, bomb and burst-jammer pulses, Vorton chain strikes,
per-shot NPC effects, jump, cloak and warp transitions, explosions and the
native missile-launch effect. Private GM preview effects are not part of the
public replay, and no animation is synthesized for a module that has no real
effect to report.

Each real sustained start records one actor-owned entry with a single expiry
at `start + duration * repeats`. When that deadline passes, or the
activation stops or is dropped first, the actor sends the authoritative stop
itself; an expiry or stop left over from an earlier activation never touches
a later start on the same module. Replays use the same 14-argument
`OnSpecialFX` event as live fire; there are no per-observer copies, no
renewal timers and no history of shots or explosions. The bookkeeping lives
in actor memory and adds no recurring database work and no immediate ship
writes; the recurring debts listed in [limitations](limitations.md) are
unchanged.

Replaying reproduces the wire values, not the client's rendering: model
loading can defer an effect, graphics families interpret the phase field
differently and network latency applies as usual, so frame-exact
synchronization with what the observer missed is not promised and has not
been exercised against a running client.

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

In space the client brain uses the live actor's prepared-fit gate when it
can supply the result. Otherwise it cold-loads the projected fit without
installing or replacing actor state. Docked reads keep the cold load.

Modifier evaluation covers item, location, group and required-skill domains,
including character-location brain items and drones. Operation 8 applies
unpenalized percent modifiers; special operation 9 derives skill level from
skill points and rank. Category-5 overload modifiers are evaluated only when
the snapshot marks a node overloaded.
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
Warp speed uses the same Fit attempt as scramble status and its projection revision; accepted requests retain that speed through alignment and flight.
Ordinary warp uses the current fitted warp capacitor need, capacity and recharge rate at activation and debits the same in-memory balance as module effects. A positive partial debit shortens that ship's route proportionally; only a full arrival can trigger the destination callback. Zero need bypasses capacitor charging, including when the capacitor is empty.
When an incoming projection makes the cached effective warp scramble status positive,
an action cancels a pending or aligning warp at the next destiny boundary. It is
fenced to the target's membership and leaves an already active warp untouched.
Hull resistance and both disruption and scrambling use the same Fit evaluation.

Once every modifier has run, the final effective value of the twelve shield,
armor and structure damage-resonance attributes is raised to the configured
`combat.resonance_floor` (default 0.05). Values consumed as further modifier
sources, such as incoming-projection and brain operands, keep the unfloored
result, while combat damage multiplies the same floored coefficients the pilot
sees.

Projections are transient, target-keyed system-actor state, indexed by source
module: each cycle replaces that module's contribution. Stop, target loss,
expiry and source or target departure remove the relevant contribution.
Finite cycles expire after their duration plus exactly one destiny tick of
grace; an indefinite duration has no expiry. Refreshes rebase the resident fit
in place inside the actor and queue kinematics through destiny. Runtime state
is lost on restart.

Queued kinematic changes accept finite positive mass, finite positive agility
that remains finite and positive as a 32-bit float, and finite nonnegative
maximum speed that remains finite as a 32-bit float. Rejected values leave the
corresponding ball property unchanged and produce no property action. Accepted
changes do not instantaneously reset or scale velocity.

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
lost projection. No real-client walkthrough is claimed by this documentation.

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

Player target locks share the surface-gap formula through one stricter gate:
admission, the scheduled completion and every existing system tick require the
pilot's current ship, a distinct visible target and a sampled surface gap
strictly inside the freshly fitted maximum target range. The running tick
drops a completed lock that fails the check, sends the usual lost payloads
and removes the matching outgoing projections; pending attempts are decided
only by their own deadlines. Always-present scene membership never extends
range. NPC targeting rules are unchanged.

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

A fitted player turret bank (a weapon group of more than one turret) resolves
each volley as one grouped shot, a server policy. The round first revalidates
the shared source and target once inside the actor; an invalid source or a
lost target cancels the volley before any consumption, roll or report, and no
member fires at a replaced identity. Each member then consumes its own charge
in module order through the existing per-member charge transaction; a failed
or empty consumption excludes that member. A member whose consumed charge type
differs from the type captured at cycle start is excluded rather than fired
with its stale packet, while a member's last charge still fires from the
packet captured before its resident charge node is removed.

Each remaining candidate is admitted once as a combat attempt owned by its
real member's replay generation and sequence. The volley rolls once inside the actor
on the master's captured roll inputs (optimal, falloff, tracking and optimal
signature) and applies the sum of admitted members' captured typed raw
damage, which already carries each module's effective multiplier. The first
admitted attempt carries it under master attribution and weapon type: a bank
whose master itself failed still reports under the master while the
contributing slave's own replay identity stands. Each observer receives one
`OnDamageMessage` with `isBanked` true and the master's weapon type,
including a miss (zero damage and quality) and a fully resisted hit (zero
damage at the rolled quality). One surviving participant still reports
banked; with no admitted member there is no roll, damage or report.

Any capacity refusal while preparing or applying a banked volley cancels the
whole round: every armed attempt closes once, nothing is damaged or reported,
and the canonical refusal stops the cycle through the normal error path. The
one exception is the mismatch fallback: a candidate whose direct-damage
kind or roll inputs differ from the captured master's, or whose consumed
charge differs from the first consumed candidate's, resolves each admitted
member per member as an ungrouped bank, logged at DEBUG with master,
member, field and both values and reported with `isBanked` false. A later
member's capacity refusal preserves earlier committed damage and reports,
then stops the cycle normally. Raw damage differences never select it; inputs
are compared exactly rather than averaged. Singletons, NPC, drone and missile
shots, and per-member capacitor, heat and shot effects, are unchanged. The
volley runs on resident inputs and actor state with no new per-cycle database
read, fit reload or member collection; each member's ammunition consumption
keeps its existing transaction, listed in [limitations](limitations.md).

The precursor disintegrator (`targetDisintegratorAttack`, SDE 6995) spools its
damage bonus over accepted shots, matching the client's `triglavianBeam.py`
repeat semantics: the k-th admitted shot applies `raw * (1 + min(k * step, maximum))`
from the fitted `damageMultiplierBonusPerCycle` and `damageMultiplierBonusMax`.
The ramp binds once per activation to the shooter's combat membership, the
activation's own generation and the locked target's incarnation; a replaced
incarnation never inherits the bonus. A rolled miss and a fully resisted
accepted hit advance it; a refused guard, an unadmitted member, a mismatched
identity or an out-of-optimal gap never fires and never advances it, and a
spooling beam breaks rather than resolving beyond optimal. A player singleton
shot returns the committed ramp and the bonus it applied; in a bank each
member's own ramp scales its own raw inside the shared turn and every really
fired member advances once.

NPC brains keep the same ramp per target-lock generation and publish the
sustained beam effect from the SDE effect's GUID on the first accepted shot.
A bound NPC shot that cannot fire ends its ramp and beam inside the same actor
turn, target loss ends both outright, and a disintegrator whose resident fit
deals no damage ends them on the capture turn. The displayed ramp —
`damageMultiplierBonusCurrent` and `damageMultiplierBonusMaxTimestamp` — is
presentation only and never persisted: bound members register it at capture,
really fired shots publish it, a stopped activation's leftovers are skipped by
the cold `ship_info` prime, and clearing reports zeroed attributes for the
closed generation only.

Missile application at impact uses target absolute speed and signature:
`sig_f = signature / max(aoeCloudSize, 1)`, and
`vel_f = (sig_f * aoeVelocity / speed)^aoeDamageReductionFactor`;
damage fraction is `min(1, sig_f, vel_f)`. Zero speed or zero explosion
velocity sets `vel_f` to one.

Vorton chain strikes apply that same missile formula in place of any turret
mechanic. EVE University Vorton mechanics, the SDE dogma operands and the
shared `missile_applied` helper are the formula authority here; no client or
third-party formula is claimed. Inside one actor settlement the primary and
every arc target are sampled independently for effective signature and
absolute velocity, and each takes its own `raw * application`: no tracking,
falloff, hit chance, random miss or wrecking roll exists. The module's packet
carries the charge's typed raw damage times the effective module damage
multiplier, plus the module's own effective explosion radius, explosion
velocity and damage-reduction factor. SDE `VortonArcRange` and
`VortonArcTargets` bound the arc search around the primary, nearest first,
the count including the primary; arc admission adds no range gate of its own.

NPC `entityChainLightning` (SDE effect 8088) classifies as the same Vorton
kind and shares this entire path: the NPC brain admits it as a tracked
effect, its packet is captured from the in-memory hull fit at the ship key
with no charge, and the same missile application and area settlement apply.
The hostile primary is prepended to the victim list explicitly; every
secondary arc reuses the broad smartbomb area policy (inclusive sampled
radius around the primary, source excluded, no per-candidate hostility,
category or cloak filter) capped by the fitted arc count. Source and primary
admission stay under `npc_projection_valid`; a stale source or target
finishes and releases the admitted attempt without applying damage.

A missile hits by actual arrival: each one-second destiny segment sweeps the
missile's path against only its designated target's path, and contact occurs
when the two ball spheres touch; the contact radius is the sum of the two
ball radii, independent of signature radius, explosion radius, third-party
balls and the `space.collisions` setting. The missile's lifetime is the
`explosionDelay` frozen at launch, in milliseconds from the moment the
launch action applies inside the system actor; a raw contact must be
strictly earlier than expiry or the flight ends in a miss. When a tick runs
late, the actor first replays the missed segments and queued actions in the
existing order and then resolves contact and expiry through one terminal
resolver, so a valid earlier contact still wins over an expiry timer that was
already due, while a departure accepted before the resolver runs still
cancels a numerically earlier contact. The hit reads the target's cached
signature, owner and membership at that actor boundary, and the target's
exact segment velocity at the contact fraction. A target's true warp,
successful cloak, accepted dock or jump, or removal cancels an unhit flight;
the launcher's true warp, successful cloak or removal cancels it too, but an
accepted dock or jump does not. A missile that loses its target or launcher
keeps flying straight until its lifetime ends and can never hit; a cancelled
flight is never re-armed, and an acceptance that later fails does not bring
the flight back. In-flight missiles are transient actor state and do not
survive a server restart.

Missiles and bombs are also damageable while they fly. The system actor keeps
one profile per live ball in its own registry, built at launch from the charge
type already loaded in the SDE and removed by the same terminal that ends the
flight. The profile is not a ship combat record: it produces no contribution,
combat fact, killmail, loot, bounty or durable loss, is absent from the ship
snapshot, and does not survive a restart. The ball's presentation health stays
the launch fraction and is not this hit-point authority. No new wire encoding
reports the remaining structure.

The profile has structure capacity only. Shield and armor capacities are zero,
so the shared layer absorber passes every point into structure and keeps the
same overflow. Structure capacity is dogma attribute `hp` (9). The four
structure resonances are `structureEmDamageResonance` (113),
`structureThermalDamageResonance` (110), `structureKineticDamageResonance`
(109) and `structureExplosiveDamageResonance` (111), in that raw-damage order.
Each value uses the shared static read: the type row, then the dogma metadata
default, then zero. A profile is refused unless
structure capacity is finite and positive and the resulting condition passes
the shared ship validator. In an actor that has an SDE, that refusal cancels
the launch before the ball or the registry entry exists and completes the
launch callback once as a miss. An actor with no SDE, used only by motion
tests, still launches a neutral ball and registers no profile. The missile
launch arguments are unchanged. A bomb's one callback now carries an optional
centre: `Some` is a real detonation, `None` is every other ending.

A bomb detonates only from its own delay resolver, and only while that flight
and its ball token are still current. The resolver takes the callback, samples
the centre, clears the profile, destroys the ball, and then reports the centre;
the callback applies the existing sampled area damage and does not destroy the
ball again. Lethal damage, scene removal, a refused launch and a stale or
replaced timer never sample a centre. Lethal damage takes the profile and the
flight callback first, destroys the ball, and completes the callback once as
a miss, so the removal hook finds an empty registry and cannot run it twice.
A missile's own contact still reports its impact after the profile is cleared;
invalidation, expiry, removal and a launch that cannot apply report a miss.
No runtime verification against a client is claimed.

A smartbomb pulse hits every in-range projectile with no friendly, ownership,
corporation, alliance or fleet filter, using the same source exclusion,
sampled position and inclusive radius as a ship. The pulse's base typed damage
is what the projectile receives; there is no turret roll. Ship plans, when the
pulse has any, are admitted and committed first. A pulse with only projectiles
uses the same source, clock and replay admission and releases its empty claim
at once. Only after that admission succeeds, and in the same actor turn, does
each listed projectile take the base damage. A refused, replayed or full pulse
finishes the attempt and leaves every projectile unchanged. A non-lethal hit
keeps the projectile in flight. A lethal hit uses the shared terminal above
and never enters a ship damage plan or loss settlement. No client walkthrough
of this pulse is claimed.

The registry and its apply operation are the one shared capability. Defender
missile selection and interception, and any later area consumer such as an
Abyssal point-defense effect, are not implemented here and must call this
capability rather than keep a second hit-point map, damage formula or flight
callback.

GMs can use `/hitchance <targetID>` to inspect each fitted turret's item and
type IDs, center and surface distances, relative speed, angular speed and its
squared value, fitted tracking, optimal signature, target signature, optimal, falloff and resulting
chance. Values have at least 17 significant digits for hand evaluation of the
native formula. Both balls are sampled together, and the source Fit includes
incoming projections. This is a diagnostic, not a client UI comparison or a shot.

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
that subtree, and the subtree's mutable attachments — fitted charges, weapon
banks, insurance contracts, blueprint state, attributes, configuration, dirt,
SKIN applications and dynamic metadata — without refunding consumed
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
