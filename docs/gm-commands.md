# GM commands

Slash commands need GM roles on the account. Grant them in the database, then log in again:

```sql
UPDATE game.accounts SET role = role | 27021597764489216 WHERE lower(user_name) = lower('<name>');
```

| Command | Effect |
|---|---|
| `/tr me <itemID\|characterID\|solarSystemID\|stationID\|celestialID>` | Teleport |
| `/wh` | Teleport next to a wormhole (scanned down for you) |
| `/wh status\|ensure\|random [count]\|clear` (alias `/wormhole`) | Show, make sure there is one, create random, or remove the wormholes of the current system. Destinations never repeat, and each batch is saved in one transaction. |
| `/scanall` | Scan every signature in the current system to 100% |
| `/sites` | List the current system's anomalies and signatures, with their signature strength |
| `/create <typeID\|name> [qty]` (alias `/createitem`) | Create items in the hangar (docked) or cargo |
| `/setisk <amount>` | Set your character's ISK wallet balance |
| `/addisk <amount>` | Add ISK to your character's wallet |
| `/setplex <count>` | Set your account's PLEX vault balance |
| `/addplex <count>` | Add PLEX to your account's vault |
| `/heal` | Repair the ship in space |
| `/dogma <ship\|itemID\|flag:N> <attribute> [value\|reset]` | Show, set or clear a per-item base value of a dogma attribute (by ID or exact name). Skills and modules still apply on top. Targets are single items only: the active ship, a module fitted at flag `N`, or any singleton item by ID. Runtime state (shield, armor and hull damage, online, charge, quantity, skills) is refused; use `/heal` or the module controls. |
| `/dogma <target> list` | List the overrides of an item; for `ship`, of the ship and every fitted module |
| `/effects unsupported` | List the module effects pilots tried to activate that the server refused, by effect and item type, since the server started |
| `/hitchance <targetID>` | For each turret fitted to your ship, show its hit chance against a ball in your system, with every input of the formula: distances, angular velocity, tracking, signatures, optimal range and falloff |
| `/giveskill <character> <skill\|all\|super> [level]` | Set a character's skill to an exact level (0-5, default 5). Training already due is completed first. |
| `/allskills [character]` | Give every published skill (default: yourself) |
| `/removeskill <character> <skill\|all>` | Remove skills from a character |
| `/givestarterskills <character>` | Add the race's starting skills the character is missing |
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

Arguments may be quoted to include spaces. Lists longer than 50 rows show the first 50 and the number left out.
