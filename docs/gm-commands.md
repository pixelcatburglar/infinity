# GM commands

Slash commands need GM roles on the account. Grant them in the database, then log in again:

```sql
UPDATE game.accounts SET role = role | 27021597764489216 WHERE lower(user_name) = lower('<name>');
```
Replies, including `/help`, appear in a private conversation with EVE System,
the fixed retail system owner; the client resolves its caption through `cfg.eveowners`.
They are delivered only while chat is online, and otherwise dropped silently
without a backlog. Bare `/` requests autocomplete without a chat reply.
Commands invoked from menus also echo their results in the conversation.


| Command | Effect |
|---|---|
| `/announce <message>` | Send a system announcement to online pilots |
| `/shutdown <minutes>` | Schedule or replace an in-memory planned shutdown: every online pilot gets the client's native shutdown countdown broadcast, and a character selected after the broadcast still gets it once when it enters the game. Inside `server.shutdown_reject_secs` before the deadline, new logins are refused; at the deadline the server takes the same teardown path as a signal, so under `restart: unless-stopped` it is an announced restart, not a maintenance stop. The plan is lost on restart. |
| `/shutdown cancel` | Cancel a plan that has not reached its deadline; online pilots get the native cancellation. With no plan it does nothing, and a due or triggering plan cannot be cancelled. |
| `/shutdown status` | Show whether a plan is scheduled, how much remains, or that it is already triggering. |
| `/tr [me] <itemID\|characterID\|solarSystemID\|stationID\|celestialID>` | Teleport the active ship. Stations relocate docked or in-space pilots into the hangar in one transaction; systems, celestials, live balls and stored space items land in space beside the target. A trailing `noblock` is accepted and ignored. Docked pilots can only target stations (station-to-space is refused), and an open direct trade blocks the move. Name lookup, coordinates, offsets, bookmarks, agents, structures and moving other pilots are not supported yet. |
| `/wh` | Teleport next to a wormhole (scanned down for you) |
| `/wh status\|ensure\|random [count]\|clear` (alias `/wormhole`) | Show, make sure there is one, create random, or remove the wormholes of the current system. Destinations never repeat, and each batch is saved in one transaction. |
| `/scanall` (alias `/sigscan`) | Scan every signature in the current system to 100% |
| `/sites` (alias `/sigs`) | List the current system's anomalies and signatures with their real site IDs and content classes (wormhole, ore, ice, gas, data, relic, mining, combat, or unrecognized). Signatures also show the site's scan strength: how readily probes resolve it, not your scan progress. Only the first 50 rows are shown. |
| `/overlayrefresh` | Resend the current system's already-loaded sensor-overlay state to your own client only, telling it to remove sites no longer present. Requires being in space with the system's scan tracker already loaded; it grants no scan authorization, generates or removes no sites, and notifies no other pilot. |
| `/create <typeID\|name> [qty]` (alias `/createitem`) | Create items in the hangar (docked) or cargo. Quantity defaults to 1; plain quantities use ASCII digits only. Case-insensitive suffixes `k`, `m`, `b`, `t` multiply by 1,000, 1,000,000, 1,000,000,000 and 1,000,000,000,000 respectively; suffixed decimals must yield an exact integer. The result must be positive and at most 9,223,372,036,854,775,807 (signed 64-bit maximum). It is the total requested, even if delivery uses more than one stack. Example: `/create Tritanium 1.5m`. |
| `/setisk <amount>` | Set your character's ISK wallet balance |
| `/addisk <signed amount>` | Adjust your character's ISK wallet by an exact signed amount; a negative amount subtracts ISK and cannot take the balance below zero. |
| `/setplex <count>` | Set your account's PLEX vault balance |
| `/addplex <count>` | Add PLEX to your account's vault |
| `/clonegrade <userID> alpha\|omega [seconds [grace_seconds]]` | Set an account's clone grade. Omega without a duration has no end date; with one, the account turns Alpha when the duration and then the grace period (0 by default) have passed. Alpha takes no duration. The account's client is told at once, and every change is audited. |
| `/heal` | Repair your active ship and all fitted modules; in space also refill capacitor and clear rack heat |
| `/dogma <ship\|me\|itemID\|flag:N> <attribute> [[=] value\|reset\|infinity]` | Show, set, clear or make unlimited a per-item base value of a dogma attribute (by ID or exact name). `me` targets your active ship, and `=` may be written in any spacing (`attr = 8`, `attr=8`, `attr= 8`, `attr =8`) or omitted. Skills and modules still apply on top. Targets are single items only: the active ship, a module fitted at flag `N`, or any singleton item by ID. Runtime state (shield, armor and hull damage, online, charge, quantity, skills) is refused; use `/heal` or the module controls. `infinity` applies only to a hull's `capacity` attribute on an assembled real ship: it removes that ship's ordinary cargo volume quota; the row keeps its real finite base while the mode is on, a finite `set` writes a new base and clears the mode, and `reset` deletes the override so the derived SDE/fitting capacity returns. |
| `/dogma <target> list` | List the overrides of an item; for `ship` or `me`, of the ship and every fitted module |
| `/effects unsupported` | List the module effects pilots tried to activate that the server refused, by effect and item type, since the server started |
| `/effect [list\|help\|?]` / `/effect <name> [me\|ballID\|target=<ballID>] [moduleType=<typeID>]` / `/effect stop\|off [<name> [ballID\|target=<ballID>]]` / `/effect stop-all` | Send a private visual-effect preview to your own client's current space connection, or stop one you sent. See the catalogue and rules below. |
| `/hitchance <targetID>` | For each turret fitted to your ship, show its hit chance against a ball in your system, with every input of the formula: distances, angular velocity, tracking, signatures, optimal range and falloff |
| `/giveskill <character> <skill\|all\|super> [level]` | Set a character's skill to an exact level (0-5, default 5). Training already due is completed first. |
| `/allskills [character]` | Give every published skill (default: yourself) |
| `/allskins [me\|characterID\|"complete name"]` | Grant the complete set of ordinary, region-visible SKINs to a character (default: yourself, works offline) in one transaction: new licenses are permanent, still-valid timed licenses become permanent, already permanent rows are skipped, and expired ones are cleaned up first. The online target gets one `OnSkinLicenseActivated` notice per changed SKIN; a repeat grants nothing. Structure, single-use and SKINR SKINs are not granted, and no license items are created or consumed. |
| `/removeskill <character> <skill\|all>` | Remove skills from a character |
| `/givestarterskills <character>` | Add the race's starting skills the character is missing |
| `/backintime [me\|characterID\|"name"]` | Irreversibly reset a character's trained skills to their race's exact starter set (default: yourself). Unlike `/givestarterskills`, which only fills in missing starter skills, this deletes every other trained skill, rewrites changed starter levels and points, clears the whole skill queue (active intervals are discarded, not banked) and sets free skill points to 0, all in one transaction. Expert systems, attributes, remap quotas, implants, boosters, wallet and location are preserved. Fitted modules that lose their skill prerequisites are taken offline; an online target is notified of the skill, queue, free-point and brain changes, while an offline target's reset persists silently. |
| `/setattr <value> [me\|characterID\|"name"]` | Set all five base attributes of a character (default: yourself). The value must be finite and nonnegative; it is rounded to the nearest integer, which must fit `i32::MAX` (so 2147483647.4 is accepted, 2147483647.5 refused). Works on online and offline targets; the target's own client is told to refetch attributes. |
| `/expertsystem add\|remove <typeID>` | Install or extend an expert system on your character without an item, or remove one. The player limits apply: three at a time, and extension only once 30 days or less remain. |
| `/ship <type> [character]` | Create and assemble a ship in the station hangar (default: yourself) |
| `/spawn <type>` | Create an assembled ship and return its item ID: in the station hangar while docked, or in space, unoccupied, `gm.spawn_distance_m` from your ship. Your active ship does not change; board the new one to fly it. |
| `/fit <character> <type\|itemID> [count]` | Fit modules by the same rules as the fitting window; they are not brought online |
| `/unload me <itemID\|typeID>` | Unfit a module of your ship together with its charges; a type ID unloads every fitted module of that type |
| `/unfit me <itemID\|typeID\|name\|all>` | Unfit high/mid/low slot modules: an item ID first, then a type ID or quoted name picks the same-type module on the lowest slot; `all` unloads every ordinary module. Rigs and subsystems stay fitted; selecting one is refused. Modules land in the station hangar or, in space, the cargo hold, whose final capacity must still fit the whole unload or nothing moves. Charges of the selected module's weapon bank return to the same destination. The same final-capacity check applies to `/unload`, so both commands refuse atomically when the cargo hold cannot take the full result. |
| `/load me <typeID> <count>` | Load charges into the compatible modules; grouped weapons load the whole bank |
| `/online <shipID\|me>` | Bring a ship's modules online, with the same CPU, powergrid, skill and group limits as the client; if one module cannot go online, none does |
| `/iteminfo <itemID>` / `/typeinfo <type>` | Show an item, or a type with its attributes |
| `/entity deploy <count> <typeID>` / `/npc <type\|group> [count] [distance]` | Spawn NPCs, at most `gm.max_spawn_count` per command |
| `/npcclear [radius\|system]` | Remove GM-spawned NPCs only |
| `/wreck [type] [distance]` / `/container [type] [distance]` | Create an empty wreck or container |
| `/sysjunkclear` | Permanently destroy every live wreck and ordinary container in the session's solar system, including everything stored inside them |
| `/testclear` | Same, limited to debris whose ball center is within 20 km of your ship in space |
| `/suicide` | Destroy your own current ship or capsule in space; no arguments. |
| `/loadsys` | Load the caller's current public solar system and the destinations of its stargates so that they are running; works docked. No arguments or performance-testing gate. |
| `/loadallsys` | Load every public solar system of the static map, including systems without stargates; no position or character needed. No arguments; performance-testing mode required. |
| `/unloadallsys` | Sweep the opening set of running systems for strict-idle unloading; no arguments, position or character needed. Performance-testing mode required. This is not the ammunition `/unload` command. |
| `/dmg <shield\|armor\|hull\|all> <percent>` | Set your ship's damage (0-100) |
| `/invu [on\|off\|status]` | Show or set the caller's persistent NPC target-ignore flag; no argument toggles, `status` only reads and writes nothing. The flag defaults to off, belongs to the selected character (never the ship, session or account), works docked and in space, and survives ship swaps, reconnects and restarts. While on, the character's piloted ship and every drone it really owns, including orphaned drones left in other loaded systems, stop being legal NPC targets, and NPC locks on them, finished or still in progress, are dropped at once; `off` restores normal NPC selection without reviving old locks. This is not damage immunity: missiles already in flight, area damage reaching the character through other targets, player attacks and shared GM forced loss all still apply. CONCORD keeps its usual rules: a response already on its way when the flag turns on does nothing on arrival, `off` does not summon a response for an offense committed while on, and the next offense is answered normally. |
| `/cwatch` or `/cwatch status` | Show your own Crimewatch state: the six combat timers (weapons, PvP, NPC, criminal, suspect and disapproval) as an exact FILETIME deadline or as off, plus the persisted safety setting. Read-only. |
| `/cwatch clear` | Turn all six timers off and cancel a CONCORD response that has not fired yet. Safety, security status, engagements and kill rights are kept; a loss already accepted or committed is not undone. |
| `/cwatch safety full\|partial\|none` | Save your own safety preference through the same writer the ordinary `SetSafetyLevel` call uses. Nothing is pushed to your client, which picks the value up at the next login, ship swap or system change, and no new safety enforcement is added. |
| `/cwatch weapon\|pvp\|npc\|criminal\|suspect\|disapproval on\|off\|<duration>` | Set one of your own timers. `on` applies its configured default duration, `off` or an exact `0` turns it off, and a duration is an absolute reset rather than an extension. Durations are ASCII decimal with an optional `ms`, `s`, `m` or `h` suffix (seconds without one), may carry a fraction such as `1.5s`, and must resolve to a whole number of milliseconds. A positive `criminal` timer set while you fly your own ship in high-security space also requests the ordinary CONCORD response, as `/naughty` does, unless NPC target ignore is on or responses are disabled; the reply shows only the timer state. |
| `/naughty` (also `/cwatch naughty`) | Refresh your own weapons and PvP timers; in high-security space also set your criminal flag and request the ordinary CONCORD response. Works only while you fly your own live ship in space. No arguments. |
| `/dirt [<0..1>\|status\|clear] [shipID]` | Show or set a hull's dirt. No argument is `status`; `clear` and `0` write the current time; `1` writes the finite saturation timestamp `0` (an ancient hull, not a clean one). The default target is your real current ship; an explicit positive ship ID accepts any physical hull, including foreign, offline, hangar-stored and persistent unpiloted hulls. Status is read-only: a hull without a stored timestamp reports uninitialized and stays unwritten. The reply reports the requested ratio, the committed timestamp and the attained level and ratio, and whether the live slim was updated, the hull had no running actor (saved for its next real load), or its incarnation changed after commit (stale: still saved, recovered on the next real load). |
| `/killmark [<count>\|status\|clear] [shipID]` / `/killmarks` | Show or set a hull's persisted player-kill count; the two names share one implementation. No argument is `status`; `<count>` is an absolute set, a nonnegative integer up to 9,223,372,036,854,775,807 (the signed 64-bit maximum), with no fractions, deltas or rounding; `clear` writes `0`. The default target is your real current ship; an explicit positive ship ID accepts any physical hull, including foreign, offline, hangar-stored and persistent unpiloted hulls. Status is read-only and never creates a row. The stored and reported value is the real count; the client's hangar model only displays up to 999. A write commits first, then reports the real count and whether the live slim was updated, the hull had no running actor (saved for its next real load), or its incarnation changed after commit (stale: still saved, recovered on the next real load). The count survives ordinary ownership transfer; only `clear`, a `0` write, repackaging or hull deletion removes it. Nothing increments it automatically. |
| `/lp [character]` | Show loyalty points |
| `/addlp <corporation> <amount>` / `/setlp <corporation> <amount>` | Add to or set your loyalty points for a corporation. The balance cannot go below zero. `/setlp` replaces only the balance observed in the same transaction; a concurrent LP change between the read and the write refuses the command instead of being folded into the target. |
| `/evermarks` / `/corpevermarks` | Show your or your corporation's exact EverMarks balance (Heraldry LP). The corporation is the character's saved membership (an NPC corporation, or a player corporation that still exists), not the one in the session. No arguments. |
| `/addevermarks <signed amount>` / `/addcorpevermarks <signed amount>` | Adjust your or your corporation's EverMarks by an exact signed 64-bit amount; a negative amount subtracts and cannot take the balance below zero. |
| `/setevermarks <amount>` / `/setcorpevermarks <amount>` | Set your or your corporation's EverMarks to an exact nonnegative 64-bit amount. |
| `/secstatus [value] [character]` | Show or set a security status (-10 to 10). Other pilots' overviews update when the ship's ball is rebuilt. |
| `/setstanding <owner> <character> <value> [reason]` | Set an owner's standing toward a character (-10 to 10); the reason is logged and recorded in the standing history. |
| `/maxagentstandings [characterID\|complete name]` (alias `/fullstandings`) | Raise the target's standing toward every corporation employing a non-deleted SDE agent and those corporations' factions to +10 in one transaction. No argument means the selected character; a positive ID or the complete (quoted) character name names another, who may be offline. Only the named union changes; agent-personal standings, other corporations, and the character's own player standings are untouched. An online target is notified per changed owner; a repeat run changes nothing. This is the raw standing value: agent access checks still apply, so a private standing of -2 or worse can still keep an agent locked. Standing history records changes only from this deployment onward. |
| `/missioncomplete [agentID\|all]` | Complete the objectives of accepted missions so they can be handed in. `all` skips branching interaction missions; naming one is refused. Existing goods in the delivery hangar, and in the active ship cargo while docked there, count toward courier and trade cargo requirements; each singleton item counts as one unit regardless of its encoded quantity. |
| `/corpcreate <name> <ticker>` | Found a corporation without the skill or the fee; the docking, name and CEO rules still apply |
| `/setalliance <name> <ticker>` | Create an alliance for your corporation; the CEO and member rules still apply |
| `/joinalliance <alliance>` | Join your corporation to an alliance without an application; the CEO and member rules still apply |
| `/session` | Show the session's attributes |
| `/jobs` | Count scheduled jobs by kind and state, and list the next ten |
| `/jobs retry <id>` | Run a job now; a stopped job gets a fresh set of attempts |
| `/jobs cancel <id>` | Delete a job. A job already running finishes but is not rescheduled. The periodic jobs come back at the next start. |
| `/lua list` | List the loaded scripts and their directories, the calls, commands, events and timers they claim (with each timer's interval and skipped runs), and their limits |
| `/lua reload` | Load the scripts again. If any script fails to load, the old set stays in use; calls already running finish on the old set. |
| `/motd [set <text>]` | Show or set the message of the day (bundled `motd.lua` script) |
| `/help` | List the commands |
| `/where` | Show your own location from the live session: the canonical solar system and, while docked, the station. The system shows its SDE name and real ID; stations have no name projection, so the station is reported by its real ID. Read-only, no arguments. |
| `/version` | Show the server build's package version and commit ID. They are the same two values as the `server build` line written to the log at start (see [build identity](development.md#build-identity)). Read-only, no arguments; the identity belongs to the process, so no selected character is required. |
| `/who` | List the pilots currently in game from the online-pilot registry snapshot (not XMPP occupants or persisted positions). The total covers the whole snapshot; only the first 50 rows are shown, with the exact number left out. Read-only, no arguments. |
| `/wallet` | Show the caller's exact character ISK and account PLEX balances; the printed caps bound the client's display, never the stored values. Read-only, no arguments. |
| `/hangar` | List your ship rows in the current station hangar (docked only): the active ship first, the rest in item order; packed rows show their exact quantity and the header counts records and total hulls separately. Only the first 50 rows are shown. Read-only, no arguments. |

Arguments may be quoted to include spaces. Lists longer than 50 rows show the first 50 and the number left out.

Loyalty-point and EverMarks commands need only the GM role; no accountant or
director corporation role is required. `/addlp` and `/setlp` freely name the
issuer corporation but always adjust the caller's own balance. The six
EverMarks commands name nothing at all: the issuer is always the Heraldry
corporation and the owner is the selected character or its real database
corporation. Amounts are exact 64-bit integers, so values above 2^53 (for
example 9007199254740993) are stored and reported exactly. Query commands are
read-only. For a write, an absolute set verifies the exact balance it observed
inside the same transaction, and a membership that moved during the
organization lock wait refuses the whole change; nothing is written and no
balance notification is sent. Committed changes send the usual `OnLPChange` or
`OnCorpLPChange` notifications, but journal no earning event and advance no
daily goals.

`/announce` sends one `OnRemoteMessage(CustomNotify)` game notification per
online pilot ID in the current snapshot, independently of XMPP/chat occupancy.
It is not MOTD: there is no offline persistence, backfill or retry. The reply
reports the snapshot target count, not delivery or read confirmation; ordinary
private slash feedback is still delivered only while chat is online. Parsed
arguments are joined with one space and trimmed at the outside; whitespace
inside quoted arguments is retained.

`/sysjunkclear` and `/testclear` permanently delete whole item trees: the wreck
or container and every item stored inside it is destroyed, never moved or
refunded, and there is no way to recover any of it. `/sysjunkclear` uses the
session's current solar system and works while docked, but only sees a running
system: one with no live actor is reported as zero roots and is never loaded, so
debris in a dormant system stays out of reach. `/testclear` requires the caller
in space on their ship and counts sampled ball centers within 20 km, boundary
included. Each run is one all-or-nothing batch: if any selected tree holds an
item the client does not allow to be trashed, or the contents changed since the
command read them, the whole command is refused, nothing is deleted, and the
reply does not identify the offending object; `/testclear` from another spot can
still clear around it. Notifications go to each row's real owner. If removal
from the live scene cannot be confirmed after the database commit, the reply
says so; the rows are already deleted in that case. Repeating either command is
safe.

`/heal` works on your own active ship only. While docked at a station it persists
the ship's shield, armor and hull condition and clears the damage of every fitted
module — high, medium and low slots, rigs and subsystems — whether the module is
online or burnt out. Unrelated attribute overrides, cargo, charges, weapon groups
and wallets are not touched; a burnt-out module stays offline. A docked ship's
full capacitor and zero rack heat come from the existing dogma projection, not
from stored fields. In space the module repair commits first, then a guarded
actor callback restores shield, armor, hull and capacitor and clears rack heat.
The callback is synchronously enqueued before the market lock is released, but
its acknowledgement is awaited without that lock; a stalled actor has no fixed
response deadline. If the original session or actor has changed after commit,
the command reports that only module damage was committed and leaves the new
target untouched. A retry patches every fitted module, even if its database
damage is already zero, to reconcile the live cache. Online, active and overload
state remain unchanged, and later cycles can accumulate heat and damage normally.
Space condition uses the existing checkpoint and transition saves; capacitor and
heat are runtime state rather than persisted repair fields.

`/suicide` takes no arguments and works only on the ship or capsule you are
currently flying in space. Docked callers and sessions whose ship or location
changed are refused, and the command never retargets a different ship. The
destruction is irreversible and runs the ordinary ship loss: an eligible
frigate parked in the ship's escape bay survives under its own item ID with
its fitting and contents, otherwise a fresh capsule appears where the ship
died, and a destroyed capsule returns you to a new capsule in your home
station's hangar. The loss writes the usual killmail, insurance and event
records. It produces no wreck, dropped loot or corpse for the pilot, and
clone body or implant loss is not modelled. `/dmg` still only sets damage
percentages.

`/dirt` maps the requested ratio through the client's own curve: `RemapDirtLevel`
is `-2 + 4r` below 0.5 and `1.4(r - 0.5)` above, and the previewer's exact 0.55
debug sentinel is an ordinary point. The timestamp is then solved from the age
formula `0.7 - 1/(weeks^0.65 + 1/2.7)`; `clear` never writes `0`. A write
commits first. A live hull gets the existing slim update; a hull without a
running actor keeps the saved value for its next real load; and a stale
incarnation keeps only persistence, recovered on the next real load. No hangar
reload or dedicated notification is sent.

`/killmark` and `/killmarks` share the same appearance write path as `/dirt`.
A write reports the committed count and one of the same three publication
outcomes: the live slim was updated, the hull had no running actor, or its
incarnation changed after commit. The stored count is never clamped: values
above 999 are kept and reported in full even though the hangar model shows
fewer marks. An identical rewrite commits without a second broadcast.

`/invu` serializes with the same market guard every ship and drone load already
holds: the flag update commits inside one transaction, the change is
synchronously queued to every live system actor before that guard is released,
and each actor's acknowledgement is awaited only after the guard is dropped.
The reply therefore means live scenes already applied the flag; a scene that
closed meanwhile keeps the saved flag for its next real load, and the reply
says so instead of claiming every lock was cleared. Enabling retires NPC
targets currently held on the pilot or owned drones; disabling only restores
future selection. NPC brains keep their aggressor memory either way.

`/cwatch` manages the caller's own Crimewatch state only. The six timers are
in-memory deadlines that follow the pilot across systems, docking and
disconnects, and they are lost when the server restarts; the suspect flag of
an active kill right is the existing exception and is restored at the next
start with its original deadline. `on` uses the operator defaults
`crimewatch.weapons_timer_secs`, `crimewatch.pvp_timer_secs`,
`crimewatch.npc_timer_secs`, `crimewatch.criminal_timer_secs` and
`crimewatch.disapproval_timer_secs`, plus `bounty.kill_right_suspect_secs` for
the suspect timer; configuration changes take effect at the next start.
`/cwatch clear` preserves safety, security status, engagements and kill
rights, and it only stops a CONCORD response that is still queued; turning
the criminal timer off cancels it the same way, while once the response's
loss is accepted or committed it is not rolled back or revived.
`/cwatch safety` persists the same preference the ordinary client call
writes; the server pushes nothing and adds no enforcement, so the client
shows the stored value at the next login, ship swap or system change and its
local restrictions still decide what each level allows. `/naughty` acts on
the caller's own live ship and has three outcomes: in high security space a
CONCORD response is queued (already pending, or queued by the call) or
suppressed (NPC target ignore is on or responses are disabled; the criminal
flag is still set), and outside high security only the weapons and PvP timers
refresh while an existing criminal flag stays. It never creates a victim,
engagement, security penalty, kill right, killmail or damage.

For `/dogma` writes, mass must be finite and positive; agility must be finite
and remain finite and positive when converted to a 32-bit float; maximum velocity
must be finite and nonnegative and remain finite as a 32-bit float. Invalid
values are refused before writing, whether the attribute is named or identified
numerically. Queries and resets remain available; reset can repair an existing
invalid override, which is not rewritten automatically.
Mass has no additional numerical floor or ceiling beyond the positive-finite
server boundary. Extreme accepted masses follow native motion math, which does
not guarantee finite motion for every finite mass.

`/dogma` reads and writes base values only. For a damage-resonance attribute the
stored base and `list` keep the written value, while the reported effective
value can sit at the `combat.resonance_floor` minimum; setting or resetting an
override never removes that floor.

`/dogma <ship|me|itemID> capacity infinity` switches the hull's persisted ordinary
cargo to unlimited: the server performs no volume quota check on that hold, and
repeating the command is a no-op. The override row keeps its real finite base
while the mode is on so a finite `set` or `list` still reports the stored value.
A finite `set` writes a new base and clears the mode; `reset` deletes the row,
restoring the normal derived capacity. Cargo already aboard is neither evicted
nor deleted. All other holds and acceptance rules (type, stack, ownership and
nested containers) stay in force, and special bays are always finite.

For `/setattr`, all five bases take the same rounded value; effective
attributes stay base plus the existing modifiers (implants, boosters, expert
systems), so an effective value can exceed `i32::MAX`. The command does not
spend or reset remap entitlements, free skill points, skill levels, implants
or boosters, and it does not move the character. Training already due under
the old attributes is settled once at the command's timestamp, then the queue
is re-timed under the new ones. A zero effective training rate leaves the
queue head active with an open end — it keeps its training intent, accrues no
SP and schedules no completion job — and it resumes from a later real change
(another `/setattr`, a modifier or booster expiry, a queue edit) without
backfilling the blocked interval. A queue the user paused stays paused.
Natural completions and expiries still happen on their own deadlines.
Deleted or unknown characters are refused; nothing is written.

`/effect` sends bounded visual-effect previews to your own client only. The
effects go out over your current space connection; no other pilot,
including the target's pilot, sees anything. Replies say what was sent,
never that the client loaded, played or removed the effect.

A bare `/effect`, or `list`, `help` or `?`, prints the full catalogue below.
It works while docked and without a selected character. Playing or stopping
requires being in space on your real current ship; the source must be visible,
not warping and not cloaked. Names are matched case-insensitively against the
canonical name, the complete GUID or any alias; punctuation folds to `_` and a
leading `effects.` is optional, so `Jump-Drive-In` equals `jump_drive_in`.

Self previews take no target (a bare `me` is accepted for compatibility).
Targeted previews accept the ball ID of a visible ship or static NPC station
in your system, as a positional number or `target=<ballID>`; `me` is not a
target. If you omit the target the command uses your own completed target
lock, but only when exactly one lock exists; with none or several the
request is refused. Playing the same effect again first stops the preview
sent before, and at most one preview per effect per ship is kept. `stop` or
`off` without a name, or `stop-all`, stops every preview your current
connection sent; only the connection that sent a preview can stop it.

The nine weapon previews also carry a firing type, given as
`moduleType=<typeID>` when it cannot be resolved alone. Candidate types are
every loaded static type, of any category, whose dogma effect GUID lists
the effect's GUID. When exactly one candidate exists it is used
automatically; when several exist the request is refused with the ascending
candidate type IDs so one can be named explicitly; when none exists
`moduleType=` is required and must name a known module-category type that
owns at least one static attribute row, and the reply notes the match could
not be checked. When candidates exist, an explicit `moduleType=` is
refused unless it is one of them. A type without its own firing-delay
attribute is still accepted, but the client then plays the effect beam-only and skips the
target impact. The request carries the type ID alone: no module is created,
fitted or activated.

Which of the nine needs an explicit `moduleType=` follows the loaded static
data, not a fixed list: with the current data superweapon_caldari,
superweapon_gallente and superweapon_minmatar resolve to one candidate,
superweapon_amarr and turbolaser link several candidate types and need an
explicit choice, and the four lance_* effects link none and need the module
fallback above.

Microjump and the nine weapon previews are one-shot pulses: each command sends
one start and they are never recorded as stoppable, so `stop`, `off`
and `stop-all` do not affect them. There is no duration or repeat option;
every request uses the client's own defaults. Previews change no gameplay
state (no module activation, damage, cloak or link state), and they are not
replayed when you change ship, system or connection.

| Canonical name | GUID | Aliases | Kind |
|---|---|---|---|
| cloak | effects.Cloak | cloakfx, cloak_effect | excluded |
| cloak_no_anim | effects.CloakNoAmim | cloaknoanim, cloak_noamim, cloaknoamim | excluded |
| cloak_regardless | effects.CloakRegardless | cloakregardless, force_cloak | excluded |
| uncloak | effects.Uncloak | decloak | excluded |
| triage | effects.TriageMode | triagemode | excluded |
| microjump | effects.MicroJumpDriveEngage | mjd, micro_jump, microjumpdriveengage | self, one-shot pulse |
| jump_in | effects.JumpIn | jumpin | self |
| jump_drive_in | effects.JumpDriveIn | jumpdrivein | self |
| jump_out_abyssal | effects.JumpOutAbyssal | abyssal_jump_out, jumpoutabyssal | excluded |
| jump_out_abyssal_between | effects.JumpOutAbyssalBetween | abyssal_jump_between, jumpoutabyssalbetween | excluded |
| abyssal_reentry_splash | effects.AbyssalReentrySplash | reentry_splash, abyssalreentrysplash | self |
| jump_drive_in_bo | effects.JumpDriveInBO | jumpdriveinbo, black_ops_jump_in | self |
| jump_portal | effects.JumpPortal | jumpportal | self |
| jump_portal_bo | effects.JumpPortalBO | jumpportalbo, black_ops_portal | self |
| capsule_flare | effects.CapsuleFlare | capsuleflare | self |
| whiteout | effects.WhiteOut | white_out | self |
| celestial_beam | effects.CelestialBeam | beam, celestialbeam | targeted |
| remote_armour_repair | effects.RemoteArmourRepair | remote_armor_repair, remotearmourrepair, remotearmorrepair | targeted |
| shield_transfer | effects.ShieldTransfer | shieldtransfer | targeted |
| data_link | effects.DataLink | datalink | targeted |
| linked_to_trace_gate | effects.LinkedToTraceGate | linkedtotracegate, trace_gate_link | excluded |
| main_bank_hacking_beam | effects.MainBankHackingBeam | mainbankhackingbeam, main_bank_beam | targeted |
| reserve_bank_hacking_beam | effects.ReserveBankHackingBeam | reservebankhackingbeam, reserve_bank_beam | targeted |
| turbolaser | effects.TurboLaser | turbo_laser | targeted weapon pulse |
| lance_amarr | effects.SuperWeaponLanceAmarr | superweaponlanceamarr, amarr_lance, azmaru | targeted weapon pulse |
| lance_caldari | effects.SuperWeaponLanceCaldari | superweaponlancecaldari, caldari_lance, steel_yari | targeted weapon pulse |
| lance_gallente | effects.SuperWeaponLanceGallente | superweaponlancegallente, gallente_lance, sarissa | targeted weapon pulse |
| lance_minmatar | effects.SuperWeaponLanceMinmatar | superweaponlanceminmatar, minmatar_lance, atgeir | targeted weapon pulse |
| superweapon_amarr | effects.SuperWeaponAmarr | super_weapon_amarr, amarr_doomsday, judgment | targeted weapon pulse |
| superweapon_caldari | effects.SuperWeaponCaldari | super_weapon_caldari, caldari_doomsday, oblivion | targeted weapon pulse |
| superweapon_gallente | effects.SuperWeaponGallente | super_weapon_gallente, gallente_doomsday, aurora_ominae | targeted weapon pulse |
| superweapon_minmatar | effects.SuperWeaponMinmatar | super_weapon_minmatar, minmatar_doomsday, gjallarhorn | targeted weapon pulse |

The eight excluded names and the client-side caveats (camera movement, link
icons, whiteout identity, firing points, locators, deferred playback) are
operator limitations; see [Known limitations](limitations.md).

`/loadsys` and `/loadallsys` load solar systems the same way a login or a
gate jump does: each target system starts running, its drones, unpiloted
ships and sovereignty are restored, and its asteroid fields are created in
the database if they do not exist yet. The commands are not read-only and
not one transaction: a system that fails to load is counted and the batch
goes on, what was written stays written, and a disconnect neither stops nor
undoes work that has started.

The public set is every solar system of the loaded static map inside the
client's known-space and wormhole ID ranges. A static row outside those
ranges is never loaded, and an ID inside the ranges that the map does not
contain is refused.

`/loadsys` takes no arguments and uses the caller's current system at the
moment the command is issued: the session's `solarsystemid2`, or
`solarsystemid` while the caller is in space. A docked session without
`solarsystemid2`, or a current system outside the public set, is refused. It
loads that system and the destinations of that system's own public
stargates, each once. Second hops, wormhole connections and player
structures are not included, and the caller is not moved. `/loadallsys`
also takes no arguments. It covers the complete public set, including
wormhole systems without stargates, and needs no selected character or
location.

When a batch starts it checks once which targets are already running; those
are counted as already loaded and skipped, and the rest are loaded one at a
time. The reply reports five counts: targets selected, already loaded at
the start, loaded during the batch (including systems another request
loaded first), failed, and not processed. A repeat whose targets all run
reports them as already loaded and starts nothing. `/loadsys` is not gated;
native `/loadallsys` and `/unloadallsys` require both the GM role and explicit
`gm.performance_testing = true` (see [Configuration](configuration.md)).
There is no full-universe load or unload at startup. Automatic idle unloading, controlled by `space.idle_unload_secs` and on by default, runs independently of these commands and of `gm.performance_testing`; see below.
While a batch runs the server handles nothing else from the caller's own
connection. Disconnecting does not cancel work already started; the final
reply may be lost. See [Known limitations](limitations.md) for the historical
measurement of a complete load.

`/unloadallsys` inspects only instances running when its batch opens. A system
started later or an actor replaced in the meantime is not stopped as the old
target. Its six counts are: opening targets, actually unloaded, busy and
skipped, changed instances, failed targets, and unprocessed targets. They add
up to the opening total; busy and changed are not successful unloads, and
failed or unprocessed counts indicate incomplete work. Only confirmed actor
and worker exits count as unloaded. A whole-operation error is reported as an
error rather than a fabricated report.

The idle check conservatively skips pilots and valid observers, accepted
arrivals or warps, future or due timers, queued actions or responses, and
queued or in-flight worker work. Drones, missiles, dynamic objects, active
projections, heat, non-NPC actor combat and sovereignty capture cannot be
discarded merely because no pilot is present. A marked belt or combat-site
cohort can leave with the actor only when every survivor is strictly stopped,
with no motion
segment, collision contact, target, lock, pending death, timer, worker,
wreck, or positive local auto-repair, including a full-hit-point ship that
still has that repair. Manual and automatic unloading share that boundary.
The survivors stay in the same process and are restored on the next cold
load before the cold scene is published; they are not respawned, rewarded,
or cleared, and a restart still drops them. Every other NPC category
stays busy. In-flight missiles already have a timer and a
nonstatic ball, covered by the existing `timers` and `dynamic_ball` debug
reasons. Unpiloted persistent ships are also busy. Reasons are debug
diagnostics, not additional reply counts.

A cold static system does not automatically generate NPCs, sites or general
gameplay timers, and ordinary static asteroid belts do not block unloading:
their persistent rocks pass the check and a field that was never depleted
keeps no timer. An existing depleted marker can restore a future regrowth
timer, and an existing Hub or capture can conditionally make a system busy.
A cohort that fails the safe check, an uncleared mining-clearance group,
other dynamic objects, drones and unpiloted persistent ships also keep a
system loaded, possibly indefinitely. Neither the busy
count nor a zero result on a second sweep is guaranteed.
The sweep uses the existing shutdown snapshot persistence path; a failed dirty
write leaves the original actor running. It does not delete asteroids, markers
or other persistent assets, and does not introduce a separate saver.

Automatic idle unloading does not go through this batch entry and is not
gated by `gm.performance_testing`: while `space.idle_unload_secs` is nonzero
(the default is 1800 seconds; `0` disables it and a nonzero value must be at
least 60) a running system that has had no pilot and no observer (including
one still receiving its initial scene) for that many consecutive seconds
gets one unload attempt through the same strict checks and single-system
stop path above. Snapshots, deferred jobs and other maintenance work inside
the system do not reset the window; only a pilot or observer does, and the
next departure starts a full window again. A system still busy stays loaded
and is tried again every 60 seconds without restarting the window, so an
unpiloted system carrying any of the busy reasons above can stay loaded
indefinitely. Automatic unloading guarantees neither that every empty system
stops nor that process memory declines. After a successful unload, the next
arrival cold-loads the system under the shared economy lock.

## SKIN administration

These are client GML tools using `shipCosmeticsMgr`, not slash commands. They require the exact GML role and operate on the current character:

- `GiveSkin(skinID, duration=<positive integer days optional>, isSingleUse=<false optional>)` grants an ordinary first-party license. Omitted duration is permanent; `isSingleUse=true` is refused. Finite renewal is server policy: add the days to the later of now and the stored expiry. A permanent grant replaces a finite license; an existing permanent license cannot be granted again.
- `GMExpireSkinLicense(skinID)` expires an existing license immediately through the shared cleanup.
- `RemoveSkin(skinID, currentCharacterID)` removes an existing license immediately through the same cleanup. Missing licenses are refused. If there are no ship applications, a GM policy message explains that an already displayed collection refreshes at next login; no ship removal event is fabricated.

`/allskins` is a slash command instead: it needs the same slash GM roles as the
other commands in this document rather than the exact GML role, and it can
target another character including an offline one. Both paths write the same
license rows and reach the same expiry cleanup, so their results intermix.
