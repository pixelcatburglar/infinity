# Lua extensions

The server loads top-level `.lua` files from `lua.scripts_dir` at startup. The
default is `crates/server/lua`, relative to the server working directory.
`[lua].mod_dirs` defaults to `[]` and adds independent script roots; entries
must not be empty. The primary root is loaded first, followed by the configured
roots, with top-level files sorted within each root. Each root uses the same
loading rules: subdirectories can contain text modules for `require`, but are
not loaded as scripts. `require` is confined to the canonical root of the
calling script and cannot resolve modules from another root.

Script names are file stems and must be unique across all roots. Duplicate names
fail startup or reload, reporting both source paths. Settings and persistent
store namespaces remain keyed by script name, not by root. Set `[lua] enabled =
false` to disable Lua entirely. Only trusted operators should install scripts:
they can call the exposed game APIs. Each file gets its own global environment,
configuration table and persistent store. Native libraries, `io`, `debug`,
binary chunks, script-created coroutines and unrestricted operating-system
access are unavailable.

The available base globals include `assert`, `error`, `ipairs`, `next`, `pairs`,
`pcall`, `xpcall`, `select`, `tonumber`, `tostring`, `type`, `rawequal`,
`rawget`, `rawlen`, `rawset`, `setmetatable`, `_VERSION` and `_G`. The
`math`, `string` (without `dump`), `table` and `utf8` libraries are available;
`os` exposes only `time`, `clock`, `date` and `difftime`.
`collectgarbage` permits only `"count"`. Modules use the same environment.

`pcall` and `xpcall` preserve ordinary Lua protected-call behavior, including
multiple return values and `xpcall`'s error handler, and support asynchronous
host calls. Typed instruction and time execution-budget failures are rethrown:
they cannot be caught by either function, and an `xpcall` handler cannot
intercept them. `require` also propagates these failures. Ordinary allocator
memory-limit errors may be caught with normal Lua semantics, but the memory cap
remains enforced for subsequent allocations. These protected calls do not
expose coroutine, native-library or additional operating-system access.

## Registration

```lua
register.rpc("exampleSvc", "GetAnswer", function(ctx, args, kwargs)
    return eve.int(42)
end)
register.command{
    name = "hello", help = "Example", gm = true,
    fn = function(ctx, args) return "ok" end,
}
register.event("CharacterSelected", function(ev)
    log.info("Selected character " .. ev.char_id)
end)
register.intercept("exampleSvc", "RestrictedMethod", function(ctx, args, kwargs)
    if not ctx:is_gm() then
        return false, "TargetingAttemptCancelled"
    end
    return true
end)
register.tick{
    name = "maintenance",
    interval_ms = 100,
    fn = function(now_ms)
        log.debug("Tick at " .. now_ms)
    end,
}
```

RPC handlers receive the invocation context and typed tuple/dictionary wire
values. A `"*"` method matches unclaimed methods; returning Lua `nil` from a
wildcard falls through. Exact handlers must return an `eve` wire value. Command
arguments are Lua strings and commands return a Lua string. `gm = true` requires
GM privileges. Duplicate RPC or command claims fail startup or reload rather
than replacing another script. Claims, events and intercepts are logged and
listed; a detected `fixed::root` RPC overlap is also logged as replacing a fixed
answer, but this does not enumerate all modeled service overlaps.

Interceptors match an exact service/method pair, with no wildcard or argument
rewriting. They receive the same live context and typed arguments as an RPC.
Returning `true` permits the call; returning `false` and a client message
catalogue key denies it with `CallError::User` and an empty dictionary. An error
or malformed result becomes `Internal`. The example above uses an existing
catalogue key, not inline player-visible text. Duplicate interceptor pairs are
load errors naming both scripts, but an RPC may claim the same pair. Interceptors
run after retail cached answers, outside Macho control paths, and before Lua RPC
claims and Rust dispatch. Existing authorization still applies. Registration
does not validate names against a route catalogue: unknown pairs can register
but never fire unless a matching call is received. For market order policies,
use actual methods such as `PlaceBuyOrder` or `PlaceMultiSellOrder`.

## Events

`register.event(kind, function(ev) ... end)` subscribes to a known kind;
unknown kinds fail loading. The one argument is a plain scalar table, not a
wire value, containing `kind` and the snake_case fields below. There is no
invocation context: event handlers cannot use context-dependent mutators.
Asynchronous `world` and `store` functions remain available.

| Kind | Fields besides `kind` | When it fires |
|---|---|---|
| `CharacterSelected` | `user_id`, `char_id` | A character is selected |
| `CharacterLoggedOff` | `user_id`, `char_id` | A character logs off |
| `Undocked` | `char_id`, `ship_id`, `station_id`, `system_id` | A ship undocks |
| `Docked` | `char_id`, `ship_id`, `station_id`, `system_id` | A ship docks |
| `Jumped` | `char_id`, `ship_id`, `from_system_id`, `to_system_id` | A ship jumps |
| `ShipLost` | `char_id`, `ship_id`, `type_id`, `system_id` | A ship is lost |
| `NpcKilled` | `char_id`, `npc_id`, `type_id`, `system_id` | A pilot kills an NPC |
| `ItemsMoved` | `char_id`, `item_id`, `from_location_id`, `to_location_id`, `quantity` | An inventory move commits |
| `WalletChanged` | `owner_id`, `account_key`, `balance`, `delta` | A wallet change commits |
| `SkillTrained` | `char_id`, `type_id`, `level` | A completed skill level is persisted |
| `MarketOrderPlaced` | `char_id`, `order_id`, `type_id`, `quantity`, `price`, `is_buy` | A market order is placed |
| `MarketOrderFilled` | `order_id`, `buyer_id`, `seller_id`, `type_id`, `quantity`, `price` | A market order is filled |
| `MissionAccepted` | `char_id`, `agent_id`, `mission_id` | A mission is accepted |
| `MissionCompleted` | `char_id`, `agent_id`, `mission_id` | A mission is completed |
| `BallparkBound` | `char_id`, `system_id` | A pilot binds to a ballpark |
| `Targeted` | `char_id`, `by_ball_id`, `system_id` | A player or NPC completes a lock on a piloted ball |
| `Damaged` | `char_id`, `ship_id`, `attacker_ball_id`, `system_id` | Damage removes shield, armor or structure from a pilot's ship |
| `ModuleCycleEnded` | `char_id`, `module_item_id`, `effect_id`, `outcome` | A module cycle ends |
| `WarpArrived` | `char_id`, `system_id` | A piloted ball changes from warp to stopped |

`type_id`, `mission_id`, `quantity`, `level` and `account_key` are i32;
other IDs, including `effect_id`, are i64, money fields are f64, `is_buy` is
boolean, and `outcome` is a string.

`ModuleCycleEnded.outcome` is `completed`, `target_lost`, `out_of_capacitor`
or `cargo_full`. One repeated module interval emits one event. A weapon bank
emits one event for each module in its captured activation snapshot. A mining
cycle emits `completed` only after its transaction commits.

Database-backed events are dispatched after COMMIT. Actor-only events are
enqueued after the authoritative actor transition. Consumers should wait for
the relevant event and read visible state rather than assume an accepted request
has completed.
Wallet values are client-facing f64 scalars, not an exact arbitrary-decimal
ledger. For GM `setisk` and `addisk`, balance is clamped to the client-facing
f64 projection and delta is projected-after minus projected-before; delta can
be zero even when the exact balance changes. The before-read can race existing
direct writers, so it is not an exact journal. Other wallet deltas are the signed
requested amounts. Existing direct core bypasses have event additions, but
arbitrary DAO writes are not globally captured.
Some autocommit shared corporation wallet writes perform an announcement or
membership lookup after changing the balance but before queuing `WalletChanged`.
If that lookup fails, the balance may be committed without a wallet event.

`NpcKilled` confirms destruction and identifies the known killer; it is omitted
when the killer is unknown. It is emitted before reward processing and does not
guarantee a successful reward or wreck. NPC kill rewards are not atomic; a failed reward after a kill can leave a partial payout.

`ItemsMoved` covers inventory moves, not every economic stack delta.
`SkillTrained` is emitted lazily when completed levels are persisted, not by a
wall-clock timer.

Transaction dispatch remains synchronous: after successful COMMIT, events are
handed off in outbox order to one ordered channel runner started by `lua::start`.
Rolled-back or retried outboxes emit nothing. Each transaction's event order is
preserved; cross-operation wall-time commit order is not promised. Delivery is
asynchronous and need not finish before the caller receives its reply. The runner
processes events and their handlers sequentially, in script load and registration
order, with independent instruction and time budgets for each handler. Failures
are logged and later handlers continue; a subscriber cannot change the committed
operation's reply. When Lua is disabled, there is no runner and events are dropped.
The event queue is best-effort and process-local, not durable across a restart.
Each event snapshots the registry when processing starts, retaining that generation
for all its handlers even if a reload occurs.

## Periodic ticks

`register.tick` registers a process-local periodic handler while the top-level
script loads. Its nonempty `name` must be unique within that script, and
`interval_ms` must be an integer of at least 100 ms. The first invocation occurs
after one interval, not immediately. The callback receives Unix time in
milliseconds as `now_ms`; it receives no `ctx`, so context-dependent mutators
are unavailable.

Ticks are not durable and do not survive a process restart. The runner executes
events and ticks sequentially, with the existing independent instruction and
time budgets for each invocation. Missed intervals are skipped rather than
queued or replayed; skip batches are warned and their counts appear in
`/lua list`. Split work that exceeds the budget into bounded chunks, keeping a
cursor between invocations; persist the cursor in `store` if it must survive
restarts.

On a successful reload, the runner adopts the replacement generation, retires
the old schedules and starts the new schedules. An already-running invocation
may finish within its existing budget; after the runner adopts the replacement,
no old tick callback starts. A failed reload leaves the current generation and
its schedules intact.

## Wire values and context

Construct wire values with `eve.none`, `eve.int(n)`, `eve.float(n)`,
`eve.bool(b)`, `eve.str(s)`, `eve.unicode(s)`, `eve.tuple{...}`,
`eve.list{...}`, `eve.dict{key=value}`, `eve.keyval{key=value}`,
`eve.rowset(columns, rows)`, `eve.descriptor(columns)`,
`eve.crowset(descriptor, rows)`, `eve.instance(class, fields)`, or
`eve.reduce(callable, args, state)`. `str` is Python 2 bytes; use `unicode` for
player-facing text. Tuples and lists are distinct on the wire. Container members
must themselves be wire values, never raw Lua tables or scalars. Descriptor
columns are `{name, type}` pairs, for example `{{"itemID", "i8"}}`.
Incoming wire values support `:int()`, `:float()`, `:text()`, `:bool()`,
`:is_none()`, `:len()`, `:items()` and `:get(key)`.

`ctx.user_id`, `ctx.char_id`, `ctx.role`, `ctx:is_gm()` and
`ctx.session:get(key)` provide invocation data; session reads return a wire
value. `ctx:notify(event, ...)` takes wire arguments; `ctx:change_session(table)`
takes string keys and wire values. `ctx:bind(service, params)` returns a wire
callback. `ctx:user_error(name, dict)` raises a client error; its dictionary
values must be wire values. Context and session objects expire when the handler
finishes or is cancelled and must never be stored for later use.

## Persistence and configuration

`store.get(key)`, `store.get_many(keys)`, `store.set(key, value)`,
`store.delete(key)` and `store.keys(prefix)` operate only on the current
script's keys. `store.get_many` takes a dense 1-based array of string keys and
returns a key-to-typed-value table in the same namespace as `store.get`.
Missing keys are absent from the table (indexing them returns nil). The read
uses one database statement, including for an empty array; decoding, size
limits and errors match `store.get`. For example:

```lua
local values = store.get_many({"cursor", "enabled"})
local cursor = values["cursor"] -- nil if absent
```

Values may be booleans, finite numbers, UTF-8 strings, nested string-keyed
tables or contiguous 1-based arrays. Nil is not a stored value. Mixed or sparse keys, cycles, functions,
userdata and non-finite numbers are rejected. An empty Lua table is stored as an
object. Nesting is limited to 64 levels; the encoded value cannot exceed
`lua.store_value_limit_bytes` (default 65536). Keys are returned in sorted order.
The table is server-owned; scripts cannot issue SQL. Reload retains stored values.
`store.set` and `store.delete` are single database statements. If a call times
out while one is awaiting the database, it may still have committed; a timeout
does not promise rollback.
`config` holds the script's own `[lua.scripts.<filename>]` TOML settings.

`messages.motd_help`, `messages.motd_saved` and `messages.motd_empty` are the
shipped MOTD messages. Player-facing text from custom scripts should follow the
server's localization policy.

## Other host functions

`log.info`, `log.warn` and `log.debug` take a string and attribute it to the
script. `time.now_filetime()` returns Windows FILETIME; `time.now_ms()` returns
wall-clock milliseconds; `time.now_tick(system)` reads the running system actor's
destiny clock and errors if the actor is not running. `sde.type(id)`,
`sde.attribute(type_id, attribute_id)`, `sde.group(id)`, `sde.category(id)` and
`sde.system(id)` read startup data; missing records return nil. `world.ball(system,
id)` returns a copied snapshot or nil; `world.balls(system, {category_id=...,
group_id=...})` returns matching sampled ball snapshots. The filters are optional
and combine with AND; a missing actor returns an empty sequence without loading
the system. `world.celestials(system)` and `world.asteroid_belts(system)` return
SDE records with `id`, `type_id`, `group_id` and three-element `position` arrays.
Missing optional SDE values are nil, and these reads do not start an actor.
`world.notify_pilot(character, event, wire_args)` sends a typed notification.
The type snapshot contains `type_id`, `group_id`, `category_id` and `name`;
group contains `category_id` and `name`, category contains `name`, and system
contains `solar_system_id`, `name` and `constellation_id`. Attribute returns
a number or nil. A ball snapshot contains `id`, `type_id`, `owner_id`,
three-element `position` and `velocity` arrays, and `radius`.

`world.schedule(system, delay_ms, op, args)` asynchronously enqueues a Rust
actor timer and returns whether the running actor accepted it; a missing or
stopped system returns false and is not loaded. Acceptance does not mean the
operation has completed. Supported operations are:

- `notify`: `{character=id, event=name, args={wire_values...}}`, with a
  contiguous typed wire-value sequence.
- `remove_ball`: `{id=ball_id}`. Scheduling raises an error for a persistent
  ship hull. If the ball becomes a persistent ship hull before the timer runs,
  removal is refused and a warning is logged. Temporary balls and drones remain
  eligible; this operation does not provide durable ship destruction.
- `spawn_npc`: `{type_id=id, corp=id, position={x,y,z}, aggro_target=id_or_nil}`.
  The type comes from the SDE and the coordinates must be finite. Spawning is
  deferred outside the actor; failures are logged asynchronously.

`world.ball` samples the current interpolated position between destiny ticks.

## Simulated pilots

`pilots` creates and controls persistent characters for the server's simulated
pilot account. It cannot control a character from a player account.

### Lifecycle, identity and list

`pilots.create{ name, race_id, bloodline_id, ancestry_id, gender_id, school_id,
station_id, balance }` creates a pilot and returns its character ID. `ancestry_id`,
`school_id` and `station_id` are optional. `station_id` is checked against static
data. First online preparation grants the normal rookie ship and fitting.

`pilots.online(character_id)` runs normal login preparation and returns `true`
for a new online transition or `false` when already online. `pilots.offline(
character_id)` performs normal departure cleanup and is harmless for an offline
pilot. An online simulated pilot appears in Local and the other system chat
channels, and in the station guest list while docked. Online, offline, dock and
undock transitions update the same presence as connected pilots.

`pilots.identity(character_id)` returns `{char_id, corp_id, ship_id, station_id,
system_id, online}`. Fields without a value are nil. It does not grant a ship or
bring the pilot online. `pilots.list()` returns every simulated pilot, online or
offline, as `{id, name}` records ordered by ID.

### Entering space

When an online simulated pilot enters space through login or undocking, the
server initializes the normal asteroid fields for that system. Persisted rocks
and normal belt growth and regrowth rules apply.

### Spatial operations

Spatial mutations require an online simulated pilot and its active in-space
ship. `pilots.undock(character_id)` and `pilots.dock(character_id, station_id)`
use the normal undock and dock rules, including docking range and weapons-timer
checks. They return no value.

`pilots.stop(character_id)`, `pilots.orbit(character_id, ball_id, range)` and
`pilots.approach(character_id, ball_id)` use normal ship movement. `range` must
be finite, non-negative and representable as a 32-bit float. `pilots.warp_to(
character_id, {x, y, z})` warps to finite coordinates. `pilots.warp_to_ball(
character_id, ball_id)` uses normal warp-in rules for an existing, uncloaked ball
in the pilot's system, or the pilot's own ship. These calls return no value and
do not move another pilot's ship when no matching actor exists.

### Targets and modules

`pilots.lock_target(character_id, ball_id)` requests a normal target lock. It
returns after the request is accepted, not after the lock completes. The lock
completes after the normal lock time, and activation on that target is refused
until then. `pilots.unlock_target(character_id, ball_id)` cancels a pending lock
or removes a completed one. Both require the online pilot's active in-space ship.

`pilots.activate(character_id, module_item_id, target_ball_id_or_nil,
repeat_or_nil)` and `pilots.deactivate(character_id, module_item_id)` use the
module's named SDE default effect. `repeat` defaults to `0`. Normal ownership,
fitting, targeting, capacitor and effect checks apply. A module with no named
default effect is refused. Deactivation normally completes the current cycle.

### Inventory, market and skills

`pilots.move_item(character_id, item_id, container_id, flag_or_nil,
quantity_or_nil)` uses normal inventory movement and returns no value. The
pilot must be online. Its current station ID selects its personal hangar; another
container ID selects an item container. IDs must be positive, `flag` non-negative
and a supplied quantity positive and representable as a signed 32-bit integer.

`pilots.sell(character_id, item_id, station_id, price, quantity, duration)`
places or fills a normal sell order and returns whether anything was sold or
listed. The pilot must be online. Price must be finite and positive, and quantity
must be positive and representable as a signed 32-bit integer. `duration = 0`
requests immediate matching. A missing item is refused. Normal station,
duration, ownership, skill, price and matching rules apply.

`pilots.skill_queue(character_id)` works online or offline and returns ordered
`{type_id, level, position, start_sp, dest_sp, start_time, end_time}` records.
`position` is zero-based and optional times are nil. `pilots.save_skill_queue(
character_id, queue)` works online or offline. `queue` is a dense one-based
sequence of records containing only integer `type_id` and `level`; an empty
sequence clears the queue and cancels its completion job.

`pilots.skills(character_id)` works online or offline and returns records ordered
by `type_id` as `{type_id, level, skill_points}`. These are the persisted skill
levels and points used by skill-queue validation.

### Reads and timeouts

`pilots.condition(character_id)` returns `{shield, armor_damage, hull_damage,
capacitor}`, or nil when no ship condition is available. It does not require an
online pilot. `capacitor` is nil without an in-space snapshot, including while
docked. `pilots.targeters(character_id)` returns incoming targeter ball IDs and
returns an empty sequence without the pilot's active ship actor. Use the world
reads above and the game-data reads below for other state.

Pilot functions are asynchronous and need no live call context. A mutating pilot
operation that has started completes even when its Lua call times out. Do not
blindly repeat a mutation after a timeout: read state or wait for relevant events,
such as `pilots.list()` after a timed-out `pilots.create`. Presence is
process-local; see [Known limitations](limitations.md).

## Game data

Reads are asynchronous: `dao.wallets.balance(character)`,
`dao.items.by_owner(owner)`, `dao.items.by_location(location)`,
`dao.items.in_hangar(character, station)`, `dao.characters.get(character)`,
`.name(character)`, `.ship(character)`, `dao.skills.levels(character)`,
`dao.corps.of(character)`, and `dao.standings.get(character, from)`.
`dao.market.bids(station, type_id)` and `dao.market.asks(station, type_id)`
return all positive remaining-volume orders of that side and type at one station
as `{order_id, price, volume_remaining, min_volume, range}` records. They are
station-local order-book reads, not regional executable-order searches.

Item reads return arrays of tables with `item_id`, `type_id`, `owner_id`,
`location_id`, `flag_id`, `quantity`, `singleton`, `group_id`, `category_id`,
and optional `custom_info`. `in_hangar` returns only the character's personal
hangar items in that station. Character `get` returns nil when absent or a table
with `name`, `corporation_id`, `race_id`, `bloodline_id`, `ancestry_id`,
`school_id`, `gender_id` and `created`; `name`, `ship`, `corps.of` and
`standings.get` return nil when absent. `skills.levels` returns a
type-ID-to-level table. Wallet balance is a number. Item-move quantities must
also fit a signed 32-bit inventory quantity.

Mutators require the live invocation context as their first argument:

- `dao.wallets.credit(ctx, character, amount)` and
  `dao.wallets.debit(ctx, character, amount)` return nil.
- `dao.items.create(ctx, owner, location, flag, type_id, quantity)` creates or
  merges a stack and returns nil.
- `dao.items.move(ctx, item, container_item, flag_or_nil, quantity_or_nil)`
  returns the destination item ID or nil.
- `dao.items.delete(ctx, item)` applies the caller's existing trash ownership
  and active-ship policy and returns nil.
- `dao.mail.send(ctx, recipient_ids, title, body, list_or_nil, corp_or_nil,
  reply_to_or_nil, forwarded_from_or_nil)` returns the message ID. Mailing
  lists remain unsupported; corporation mail follows the native service policy.
- `dao.standings.set(ctx, character, from, value)` returns nil; standings must
  be finite and between -10 and 10.

IDs and quantities must be positive and amounts finite and positive. Mutations
use the server's service operations and transaction/outbox rules. Once a
multi-step mutation has been dequeued, the execution deadline cannot interrupt
its transaction and post-commit effects: it finishes before a timeout is
reported, and the script is not resumed after its deadline.

## Limits and administration

Defaults are 64 MiB VM memory, 1000 ms and 5,000,000 instructions per call,
with an instruction hook every 1000 instructions. Configure these with
`memory_limit_mb`, `call_budget_ms`, `call_budget_instructions` and
`instruction_hook_interval` under `[lua]`. Do not raise the call budget to
process every pilot each tick. Persist a cursor in `store` and process a bounded
slice per tick instead. `/lua list` shows each script's canonical root, RPC and
command claims, events, intercepts, ticks with their intervals and skip counts,
and limits; `/lua reload` builds a fresh registry and swaps it only on success.
In-flight handlers retain their old registry; Lua globals reset, but stored
values survive.
The shipped `/motd` command is GM-only:
`/motd set <text>` saves a message and `/motd` reads it. `CharacterSelected`
sends the saved message to the registered pilot on both docked and space logins.

The bundled `fixed_answers.lua` claims five reference methods. The client reads
the information-gathering keyval when that service starts. Character rename
details are requested only when the rename feature is enabled; localization
QA-wrap requires the QA role, and text changes require an unpackaged client
with localization hashes. The anti-cheat canary query requires its feature
switch. These conditional claims are not evidence that five calls occur in
every login or docking walkthrough.

Test scripts against representative typed arguments, malformed inputs and resource
limits, and run the release tests. The persistent store database fixture requires a
separate initialized PostgreSQL database; see its test for the explicit environment
variable. Validate player-visible behavior with the real client.
