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
| `/tr me <itemID\|characterID\|solarSystemID\|stationID\|celestialID>` | Teleport |
| `/wh` | Teleport next to a wormhole (scanned down for you) |
| `/wh status\|ensure\|random [count]\|clear` (alias `/wormhole`) | Show, make sure there is one, create random, or remove the wormholes of the current system. Destinations never repeat, and each batch is saved in one transaction. |
| `/scanall` | Scan every signature in the current system to 100% |
| `/sites` | List the current system's anomalies and signatures, with their signature strength |
| `/create <typeID\|name> [qty]` (alias `/createitem`) | Create items in the hangar (docked) or cargo. Quantity defaults to 1; plain quantities use ASCII digits only. Case-insensitive suffixes `k`, `m`, `b`, `t` multiply by 1,000, 1,000,000, 1,000,000,000 and 1,000,000,000,000 respectively; suffixed decimals must yield an exact integer. The result must be positive and at most 9,223,372,036,854,775,807 (signed 64-bit maximum). It is the total requested, even if delivery uses more than one stack. Example: `/create Tritanium 1.5m`. |
| `/setisk <amount>` | Set your character's ISK wallet balance |
| `/addisk <signed amount>` | Adjust your character's ISK wallet by an exact signed amount; a negative amount subtracts ISK and cannot take the balance below zero. |
| `/setplex <count>` | Set your account's PLEX vault balance |
| `/addplex <count>` | Add PLEX to your account's vault |
| `/clonegrade <userID> alpha\|omega [seconds [grace_seconds]]` | Set an account's clone grade. Omega without a duration has no end date; with one, the account turns Alpha when the duration and then the grace period (0 by default) have passed. Alpha takes no duration. The account's client is told at once, and every change is audited. |
| `/heal` | Docked at a station: repair your active ship's shield, armor and hull and every fitted module, including offline, burnt-out, rig and subsystem slots. In space: repair the ship's shield, armor and hull |
| `/dogma <ship\|itemID\|flag:N> <attribute> [value\|reset]` | Show, set or clear a per-item base value of a dogma attribute (by ID or exact name). Skills and modules still apply on top. Targets are single items only: the active ship, a module fitted at flag `N`, or any singleton item by ID. Runtime state (shield, armor and hull damage, online, charge, quantity, skills) is refused; use `/heal` or the module controls. |
| `/dogma <target> list` | List the overrides of an item; for `ship`, of the ship and every fitted module |
| `/effects unsupported` | List the module effects pilots tried to activate that the server refused, by effect and item type, since the server started |
| `/hitchance <targetID>` | For each turret fitted to your ship, show its hit chance against a ball in your system, with every input of the formula: distances, angular velocity, tracking, signatures, optimal range and falloff |
| `/giveskill <character> <skill\|all\|super> [level]` | Set a character's skill to an exact level (0-5, default 5). Training already due is completed first. |
| `/allskills [character]` | Give every published skill (default: yourself) |
| `/removeskill <character> <skill\|all>` | Remove skills from a character |
| `/givestarterskills <character>` | Add the race's starting skills the character is missing |
| `/expertsystem add\|remove <typeID>` | Install or extend an expert system on your character without an item, or remove one. The player limits apply: three at a time, and extension only once 30 days or less remain. |
| `/ship <type> [character]` | Create and assemble a ship in the station hangar (default: yourself) |
| `/spawn <type>` | Create an assembled ship and return its item ID: in the station hangar while docked, or in space, unoccupied, `gm.spawn_distance_m` from your ship. Your active ship does not change; board the new one to fly it. |
| `/fit <character> <type\|itemID> [count]` | Fit modules by the same rules as the fitting window; they are not brought online |
| `/unload me <itemID\|typeID>` | Unfit a module of your ship together with its charges |
| `/load me <typeID> <count>` | Load charges into the compatible modules; grouped weapons load the whole bank |
| `/online <shipID\|me>` | Bring a ship's modules online, with the same CPU, powergrid, skill and group limits as the client; if one module cannot go online, none does |
| `/iteminfo <itemID>` / `/typeinfo <type>` | Show an item, or a type with its attributes |
| `/entity deploy <count> <typeID>` / `/npc <type\|group> [count] [distance]` | Spawn NPCs, at most `gm.max_spawn_count` per command |
| `/npcclear [radius\|system]` | Remove GM-spawned NPCs only |
| `/wreck [type] [distance]` / `/container [type] [distance]` | Create an empty wreck or container |
| `/dmg <shield\|armor\|hull\|all> <percent>` | Set your ship's damage (0-100) |
| `/lp [character]` | Show loyalty points |
| `/addlp <corporation> <amount>` / `/setlp <corporation> <amount>` | Add to or set your loyalty points for a corporation. The balance cannot go below zero; an LP reward arriving between the read and the change of `/setlp` still counts. |
| `/secstatus [value] [character]` | Show or set a security status (-10 to 10). Other pilots' overviews update when the ship's ball is rebuilt. |
| `/setstanding <owner> <character> <value> [reason]` | Set an owner's standing toward a character (-10 to 10); the reason is logged |
| `/missioncomplete [agentID\|all]` | Complete the objectives of accepted missions so they can be handed in. `all` skips branching interaction missions; naming one is refused. |
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
| `/who` | List the pilots currently in game from the online-pilot registry snapshot (not XMPP occupants or persisted positions). The total covers the whole snapshot; only the first 50 rows are shown, with the exact number left out. Read-only, no arguments. |
| `/wallet` | Show the caller's exact character ISK and account PLEX balances; the printed caps bound the client's display, never the stored values. Read-only, no arguments. |
| `/hangar` | List your ship rows in the current station hangar (docked only): the active ship first, the rest in item order; packed rows show their exact quantity and the header counts records and total hulls separately. Only the first 50 rows are shown. Read-only, no arguments. |

Arguments may be quoted to include spaces. Lists longer than 50 rows show the first 50 and the number left out.

`/heal` works on your own active ship only. While docked at a station it persists
the ship's shield, armor and hull condition and clears the damage of every fitted
module — high, medium and low slots, rigs and subsystems — whether the module is
online or burnt out. Unrelated attribute overrides, cargo, charges, weapon groups
and wallets are not touched; a burnt-out module stays offline. A docked ship's
full capacitor and zero rack heat come from the existing dogma projection, not
from stored fields. In space the command restores shield, armor and hull only;
module damage, capacitor and heat are not yet repaired there.

For `/dogma` writes, mass must be finite and positive; agility must be finite
and remain finite and positive when converted to a 32-bit float; maximum velocity
must be finite and nonnegative and remain finite as a 32-bit float. Invalid
values are refused before writing, whether the attribute is named or identified
numerically. Queries and resets remain available; reset can repair an existing
invalid override, which is not rewritten automatically.
Mass has no additional numerical floor or ceiling beyond the positive-finite
server boundary. Extreme accepted masses follow native motion math, which does
not guarantee finite motion for every finite mass.

## SKIN administration

These are client GML tools using `shipCosmeticsMgr`, not slash commands. They require the exact GML role and operate on the current character:

- `GiveSkin(skinID, duration=<positive integer days optional>, isSingleUse=<false optional>)` grants an ordinary first-party license. Omitted duration is permanent; `isSingleUse=true` is refused. Finite renewal is server policy: add the days to the later of now and the stored expiry. A permanent grant replaces a finite license; an existing permanent license cannot be granted again.
- `GMExpireSkinLicense(skinID)` expires an existing license immediately through the shared cleanup.
- `RemoveSkin(skinID, currentCharacterID)` removes an existing license immediately through the same cleanup. Missing licenses are refused. If there are no ship applications, a GM policy message explains that an already displayed collection refreshes at next login; no ship removal event is fabricated.
