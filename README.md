# Infinity

A server emulator for the EVE Online **Infinity** client (build 3653293, machoNet 496), written in Rust.

A single process acts as both proxy and sol node. It also serves XMPP chat, the image server, the public gRPC gateway and the content CDN. Game state is stored in PostgreSQL.

> This project is not affiliated with or endorsed by CCP Games. EVE Online is a trademark of CCP hf.

## Features

- **Login and characters.** GPS handshake with the Placebo crypto pack. An account is created the first time a new user name logs in. Character selection and creation. Every account is treated as Omega. A character with no active ship, such as a new one, receives their race's rookie ship fitted with online civilian modules (a gun, a miner, an afterburner and defensive modules) and one Civilian Hobgoblin in the drone bay. The server chooses this fit from the civilian module line; it is not a copy of the live server's. Civilian guns need no ammunition. An existing ship is never refitted or restocked.
- **Stations and items.** Inventory and hangars, and the assets window with asset search. An item can be moved only where the pilot can reach both ends: when docked, the current station's hangars, the pilot's corporation office there, and the ships and containers inside them; in space, the active ship, an owned ship with a fleet hangar or ship maintenance bay nearby, and a cargo container or wreck within its operational distance. Boarding, assembling and repackaging ships. Repackaging a fitted ship puts its modules, subsystems, drones, cargo and ammunition in the station hangar; rigs cannot be unfitted, so they are destroyed once the player confirms the client's warning, except on strategic cruisers, whose rigs go to the hangar too. Saved fittings, ship insurance, jump clones and reprocessing. Station repair: the quote covers the armor, hull and module damage of the selected items, and the ISK is paid in the same transaction as the repair. A selection without modules can be repaired in part; one that includes a module must be paid in full. Docking does not repair a ship. Private notes in the info window.
- **Direct trades.** Two online pilots docked in the same station can trade ISK and items from their personal hangars, including partial stacks and containers or inactive ships with their contents. Both must accept the exact offer, and any change resets both acceptances; the exchange then happens in one transaction. Cancelling, leaving the offer unchanged for `trade.idle_timeout_secs` (30 minutes by default), disconnecting, undocking, clone jumping or a server restart returns the offered items to where they came from. Offered items cannot be moved, split, stacked, renamed or trashed until the trade ends.
- **Dogma.** Effective attributes are computed on the server from the character, skills, hull, modules, charges, drones, and the implants and boosters in the character's own slots, including stacking penalties. Passive skill, implant and booster bonuses are also sent to the client as its character brain, so the fitting window and the HUD show them; the brain is sent again whenever skills, implants or boosters change. Module activation goes through one registry of SDE effect IDs and one activation cycle, shared by weapons, local repairs, propulsion and mining. Modifiers the client defines only in Python are added at start to effects whose SDE modifier list is empty. Fitting follows the client's rules: slots and hardpoints, calibration, rig size, subsystems, hull and group restrictions, the most modules of a group that may be fitted, online or active at once, and required skills. A module that needs more CPU or powergrid than the ship has left can be fitted, but not brought online. When a ship's load goes above its CPU or powergrid output, for example after a skill is removed or extracted, an implant is destroyed or a booster expires, through a `/dogma` override, or when a ship fitted before these checks is boarded, modules go offline in the client's order until the load fits. See [Effects](docs/effects.md) for the registry, what is supported, and how to add an effect family.
- **Space.** Undock and dock, approach, align, orbit and follow, warp, and stargate jumps. A pilot can board an unoccupied assembled ship they own whose surface is within 6,550 m of their ship's, from a capsule or another ship; the ship they leave stays in space, stopped and unpiloted, with its own fitting, cargo and condition, and is still there after a restart. Each solar system runs as its own actor, so systems advance independently and never block each other. Ships move on the client's one-second destiny tick: a command is stamped with the tick the system is on and takes effect before the next step, as on the client. Go-to and align, the slow-down on arrival, approach, follow, orbit, stop, missile flight, and warp alignment and flight use carbonengine destiny's mechanics and integration, so other pilots see the same course. Ships bump off each other and off stations, gates and asteroids with destiny's sphere collision, and undocking, gate arrival and GM teleports place a ship outside the collision sphere the client is sent. A stargate jump plays the jump tunnel during its four-second wait, before the ship moves to the next system. The ship arrives cloaked, as does a ship whose pilot logs in while in space; the cloak drops, visibly for every pilot on grid, when the pilot moves, stops, warps, activates a module or starts locking a target, or after 60 seconds. Docking is accepted only when the ship's surface is within 2500 m of the station's; the ship then stops, and enters the station 5.5 seconds after the client is told the request was accepted. Belt rats and the contents of a scan site appear when the ship actually lands from warp. Timers and NPCs run every 100 ms and read positions interpolated between ticks, as the client draws them. A per-system pilot cap with a jump queue is optional.
- **Combat.**
  - Targeting, ammunition, weapon groups, turrets and missiles, and capacitor.
  - Weapon groups: online guns of the same type and ammunition can be linked into a bank in slot order, and banks can be merged, peeled, unlinked and destroyed. A bank fires and reloads as one, and activating a slave fires its master's bank. Taking any member offline destroys its bank; dragging a fitted gun out of its slot removes it from its bank, or destroys the bank if it is the master. Bank membership is saved; a firing cycle is not.
  - Turret hit chance follows carbonengine destiny's accuracy formula, for ships and NPCs: angular velocity against tracking and signature, and surface distance against optimal range and falloff. Missile damage is scaled by the target's signature radius and speed, at the moment the missile hits, against the missile's explosion radius and velocity. NPCs with no falloff hit only inside optimal range, and NPCs with no tracking ignore angular velocity.
  - Electronic warfare from ships and NPCs: stasis webifiers, warp scramblers and disruptors, target painters, sensor dampeners, tracking and guidance disruptors, remote sensor boosters and remote tracking computers, including their scripts. A scrambled ship cannot warp, and scrambling shuts off microwarpdrives. Tracking and guidance disruption shorten an NPC's range and lower its hit chance and missile damage. Targeted ECM and burst jammers work against the target's strongest sensor type: the chance is the jammer's strength for that type against the sensor's strength, with optimal range and falloff. A jammed ship or NPC loses its locks and cannot lock again until the jam ends, after the module's jam duration or its cycle time.
  - Capacitor warfare from ships and NPCs: neutralizers, nosferatu and capacitor transfer, with optimal range and falloff. Capacitor boosters use their charges, and ancillary shield boosters and armor repairers use their charges and reload from cargo automatically when they run dry. An NPC transfers capacitor only to another NPC of its own spawn group, or with the same owner, never to its player target.
  - Remote shield, armor and hull repair from ships and NPCs, with optimal range and falloff: shield lands at the start of a cycle, armor and hull at the end, as for local repair. Ancillary remote repairers use their charges and reload from cargo. An NPC repairs the most damaged ally of its own spawn group or owner, never its target. Remote assistance, capacitor transfer included, does not give Crimewatch flags yet.
  - Drones launch from the bay and can engage, mine, return to orbit or to the bay, assist or guard a fleet member, and be abandoned or scooped. Launch count, bandwidth, bay capacity, control range and drone attributes come from the same fitting evaluator as ships, with the owner's skills. Combat, logistics and electronic-warfare drones use the same effects as modules and NPCs, and their attacks count as their owner's. Mining drones fly to the rock and bring the ore back to the hold. Aggressive idle drones answer legal attacks on their owner: with focus fire on, they all take the attacker with the lowest ball ID; otherwise idle drones share the attackers in drone-ID order. A command from the pilot always takes precedence.
  - Smartbombs, bombs and Vorton projectors. A smartbomb, fitted to a ship or an NPC, damages the ships, drones and NPCs in range around it. A bomb flies ahead of the launching ship and detonates after its flight time. A Vorton projector hits a locked target and chains to further targets in range of it, up to the fitted target count.
  - Cloaking devices with the client's activation rules and targeting delay, micro jump drives, tractor beams, cargo and ship scanners, survey scanner cycles, and automatic and passive targeting. A tractor beam pulls a wreck or container to the ship's surface. Compression modules advertise their fitted types and range.
  - Overheating: modules that support it can be overloaded one at a time, by rack, or as a weapon bank. A running module starts overloading at its next cycle and stops after its current cycle, and an idle module can be set to overload before it is activated. Overloaded cycles build rack heat, which can damage the online modules in that rack and burns a module out at its structure hit points; a paid station repair restores it. The damage chance follows EVE University's published overheating research, because the client has the rack heat curve but not the damage roll. Rack heat and overload settings are not saved: they reset on docking, jumping, logging off, leaving the ship and restart, and warp keeps them. Resetting heat on docking is a server policy; on retail, rack heat keeps cooling while docked.
  - Salvagers: the chance is the wreck's access difficulty against the salvager's bonus. A successful salvage claims the wreck once and leaves its loot in it. Wreck yield tables do not ship with the client, so it gives `salvage.salvage_material_stacks` stacks of a random published salvaged material instead (see Configuration).
  - NPC AI, bounties, wrecks and loot.
  - Crimewatch: timers, criminal flags, security status, CONCORD and killmails.
  - Player bounties on characters, corporations and alliances, with contributions, leaderboards and search. A pilot who destroys a target's ship is paid from the matching bounty pools, at most 20% of the destroyed value, once per killmail.
  - Kill rights: illegal aggression earns the victim an expiring right against the aggressor, which the victim can activate or sell to anyone or to one pilot. Activating it makes the aggressor a suspect anyone may shoot; the right is used up when the aggressor dies while it is active.
- **Skills.** A persistent training queue completes through scheduled jobs, including after a restart. Skill history and attribute remaps are saved, and a saved queue is validated as the client does. Skillbooks, free skill points, injectors and extractors can be used, and large and small injectors can be split or combined; item use and the skill-point change commit together. Skill plans are served through the public gateway.
- **Implants and boosters.** Implants can be plugged in and boosters consumed from the pilot's own hangar or active ship. They apply to fitting and character attributes, and a booster's rolled side effects are saved with it. Booster expiry and skill training completion run on one durable timeline per character, including after a restart, so points trained before a booster expires are counted at the old rate.
- **Economy.**
  - Regional market: seeded NPC orders, escrow, taxes and fees, history.
  - Contracts: item exchange, auction and courier, for pilots and for corporations. A corporation contract is created, accepted or bid on from a wallet division the member may spend from, and refunds and settlement use that same division; its items come from an office division the member may take from, or from the station's Deliveries hangar. Items a corporation receives, including the items it asked for in an exchange, arrive in Deliveries, and returned items go there too when their office no longer belongs to the corporation. Requested items are handed over as they are, so a blueprint copy, an assembled ship or a container with its contents arrives unchanged. Blueprints of installed or ready industry jobs cannot go into a contract or be handed over. An overdue courier stays in progress until someone fails it, and can still be delivered. A repeated or out-of-date action on a contract changes nothing, and a failed auction settlement is retried.
  - Industry: manufacturing, research, copying and invention at NPC stations, for pilots and for corporations. A corporation job takes its blueprint and materials from office divisions the installer may take from, delivers into the chosen division, and is paid from the installer's wallet or from a corporation division they may spend from. A job becomes ready at its end time, including after a restart, and is delivered when the pilot asks; until then its blueprint cannot be moved, trashed, sold or traded. Station taxes and the system cost index come from the industry configuration, the station's tax settings are read-only, and a job whose quoted cost or tax no longer matches is refused.
  - Mutaplasmids: applying a mutaplasmid to a compatible module or drone consumes both and creates one mutated item. Each mutated attribute is drawn uniformly from the mutaplasmid's range, and the result keeps the source's own attributes with those factors applied, so fitting and Show Info use them, including after a restart. The inputs may be in the station hangar, the active ship or an owned container there; an item in a corporation office hangar also needs the division's take role. Mutaplasmid ranges and compatible types come from the imported static data, and a mutaplasmid with missing or malformed data is refused.
- **Agents.** Kill missions, epic arcs, NPC standings, loyalty points and LP stores. LP store offers come from a catalogue the operator imports from ESI with `lp-import` (see [LP offer import](docs/lp-import.md)); the server itself never contacts ESI. While the catalogue is empty, every NPC corporation offers its race's faction items, priced from retail values, and the server logs a warning at start. You can buy an offer for yourself or, with withdrawal access to the chosen wallet division, for your corporation, which then pays the LP and ISK; the required items always come from your hangar in the station.
- **Exploration.** Anomalies and signatures, probe and directional scanning, wormholes, the data and relic hacking minigame, and belt rats.
- **Star map.** The jumps, ship kills, NPC kills, pod kills and visited-systems filters show what happened on the server. A gate or wormhole arrival counts one jump for the system the pilot arrives in, and one visit to it for that pilot; leaving a system, failed jumps and GM teleports do not count. Player ship and capsule losses, NPC deaths and the destruction of unpiloted ships count once each in their system; drones do not count. The filters cover the last hour or the last 24 hours, and a pilot's visits are kept for good. If the server stops right after an arrival or an NPC death, that event can be left uncounted.
- **Mining.** Asteroid belts, ore, ice and gas, mining modules, and belt regrowth. The personal mining ledger lists every delivery from mining modules and drones, with the residue wasted; the window shows new entries when the client next reloads it. An active ship compressor lets its pilot and the pilot's fleet compress items in space: a stack in the active ship's cargo or mining holds, of a type the compressor's fitted lists accept and within its range measured surface to surface, becomes the same number of units of its compressed type in the same hold.
- **Social.** Chat (local, corporation, fleet and other channels), fleets, EVE mail and notifications, the calendar, and bookmarks. The server decides which chat channels a pilot may join: local, corporation, alliance, faction and fleet channels follow the pilot's current session, whatever room name the client asks for. Pilots can create their own channels, which only members can join unless the owner makes them public, and private chats between two pilots. Owners and managers invite members, set the message of the day, mute and ban, for a set time or permanently, and delete the channel; channels, their members and moderation survive a restart. Fleets have wings and squads, member options (free move, the automatic squad, the size limit and each member's consent to fleet warps), bulk moves and wing arrangements by the leader, and the fleet finder: the server decides who may see and apply to an advert from current affiliations, the leader's contacts and blocks and the applicant's security status, and an accepted application becomes a normal invitation. Fleet, system and bubble broadcasts follow the fleet hierarchy. Members on the watchlist report their damage to the watcher anywhere in space. A commander's fleet warp takes every member under them in the same system who accepts fleet warps, each through their own ship's warp and scramble checks. Personal contacts: pilots, corporations, alliances and agents with a standing and a watchlist flag, and a separate block list; adding or editing a contact can notify the other pilot. Contacts show as online or offline, and pilots on a watchlist announce when they log on or off. Bookmark folders are personal or shared. In the access lists a pilot creates groups of pilots, corporations, alliances or everyone, each entry a member, a manager, an admin or excluded; an excluded entry wins, otherwise the most specific entry decides, and a group's last admin cannot be removed. A shared folder names a group for each of four roles: view reads and copies its bookmarks, use adds bookmarks and subfolders and edits the pilot's own, manage edits, moves and deletes any of them, and admin changes the folder and its groups. Pilots add shared folders to their known folders, and open clients update as soon as a folder changes or a pilot loses access; groups, folders and known folders survive a restart.
- **Corporations and alliances.**
  - Player corporations: founding, applications and invitations, roles and titles, a bounty tax paid into the corporation wallet, and bulletins, contacts and labels.
  - Seven wallet divisions with ISK transfers between characters and corporations. Offices rented per station, with corporation hangars and rent bills that can be paid automatically. Directors, the CEO, accountants, junior accountants and traders can see each station's Deliveries hangar, and all of them except junior accountants can take items from it; nothing can be put into it.
  - Shares, dividends and votes. A corporation is founded holding all of its own shares (`corporations.initial_shares`, 1000 by default); corporations that existed before this version receive them once at the next start, and changing the setting later does not alter existing corporations. A director or the CEO gives the corporation's shares to pilots and other corporations, and pilots move their own. The CEO pays dividends from the master wallet division to every holder except the corporation itself, each rounded down to the cent, with the remainder kept. A director or the CEO opens general votes; each ballot weighs the shares its pilot held, or held through a corporation they lead, when the vote opened, and a holder counts once. A vote closes on time, including after a restart, and its result is the weighted total.
  - Alliances: founding, applications, member removal, executor support, and bulletins and contacts.
  - Mutual wars: a director or the CEO of a corporation outside an alliance, or of an alliance's executor corporation for the alliance, can invite another such corporation or alliance to a war. An invitation lasts seven days and can be accepted, declined or withdrawn, and an owner can block invitations. An accepted war costs nothing and starts 24 hours later. Either side can retract it; the two stay at war for a 24-hour cooldown, and then the war ends. Between owners at war, attacks are legal and bring no criminal flag, security loss or kill right; a pilot fights as their alliance, or as their corporation outside one. Each kill report records the wars it counts for and the alliances of the victim and the final blow at that moment, so a war's report and kill statistics stay the same when pilots or corporations change affiliation. Invitations, wars and their deadlines survive a restart.
- **Store.** New Eden Store catalogue, PLEX vault, redeeming queue and seasonal login rewards.
- **Lua scripts.** Scripts in `crates/server/lua/` can answer remote calls and add GM commands without changes to the server's routing. A script can also intercept one exact remote call before the server answers it, for example to refuse a buy order with a message, and react to game events such as a character being selected, a ship being lost, a market order being filled or a mission being accepted. Events reach scripts in order, after the change that caused them has been saved. The bundled scripts keep a message of the day that a GM sets, sent when a character is selected, and answer five client calls that only need fixed answers. See [Lua scripts](docs/lua.md).
- **GM slash commands** (see [GM commands](#gm-commands)).

## Known limitations

- The new player tutorial (AIR NPE) cannot run. Its dungeon layouts and behaviour trees never shipped with the client, so new characters start in their school's station.
- Not supported:
  - wars declared by one side, which need a war headquarters structure; war allies and surrender; sovereignty, and alliance capital systems and prime time
  - wallet journals; votes on a new CEO, war, share issue, expulsion or item locks, and acting on a vote's result; dividends paid equally to members
  - reactions and industry at player structures; NPC stations offer no reaction facility in the client
  - fleet boosts, loading a saved fleet setup, removing unqualified members when arranging a wing by ship type, fleet warp to a bookmark, and cynosural beacon broadcasts
  - beacon counts and faction-warfare kills on the star map
  - in chat: password-protected channels, granting channel access to a whole corporation or alliance, taking over ownership of a system channel, and the local chat blackout in null-security space
  - access groups owned by a corporation, and access lists for structures
  - clone-grade and clone-implant restrictions and planetary colony extraction rules: every character is treated as Omega, and implants stay with the character rather than a jump clone
  - modules whose effect is not implemented yet, among them mutadaptive remote armor repairers. Activating one is refused with a message, and `/effects unsupported` lists the effects tried since the server started.
  - in direct trades: items from cargo, from inside a container or from a corporation hangar, the active ship, fitted modules and charges, and PLEX; types that cannot be traded are refused anywhere in an offered container or ship
  - the GM bounty reimbursement and bounty cache calls
  - drone salvage, and reconnecting to drones whose control was lost; a lost drone stays its owner's and comes back with Return to Drone Bay, and an abandoned drone can be scooped
  - labels for personal contacts
  - LP store offers that cost Analysis Kredits: they are listed, but buying one is refused
  - nanite repair paste, heat repair while tethered, NPC overload, and repairs at structures
  - mutating items that are fitted, damaged, deployed in space, in a ship maintenance bay or owned by a corporation, and mutaplasmids that need extra inputs; mutated items cannot be renewed or reset
  - automatic compression, and compression at stations and structures; turning automatic compression on or off is refused with a message
  - survey scan results pushed by the scanner: the mining overlay gets them through its own scan call
  - corporation mining ledgers for moon-drilling structures
  - loot rights: any pilot whose ship is within a wreck's or cargo container's operational distance (2,500 m unless its type sets another) can take from it, and taking from someone else's does not make the pilot a suspect
  - moving items through another pilot's fleet hangar or ship maintenance bay, and control towers
  - boarding a ship in space that someone else owns, and taking over its contents
  - wrecks, insurance payouts and killmails for an unpiloted ship destroyed in space; the ship and its contents are deleted
- An industry job installed before this version whose blueprint has since been moved or destroyed is refused at delivery.
- Mining and salvage drones have no mass or agility in the static data; the server gives them the common light-drone values (3000 kg, agility 100). Drones with no targeting range or scan resolution in the static data lock at once within control range.
- The personal mining ledger starts with mining done after this version is installed.
- A corporation at war that joins an alliance keeps its war, but the war no longer applies to its pilots, who now fight as the alliance, as the client also shows. The corporation's own CEO or directors can still retract it.
- Kill reports written before this version have no alliance recorded for the victim and the final blow, and these stay empty.
- In a war report the client lists a kill under a side only if the final blow came from that side, so a kill whose final blow came from a third party counts in the war's totals but appears in neither side's list.
- Fleet bubble broadcasts and fleet warp treat a whole solar system as one grid, because the server keeps one ballpark per system.
- A pilot who changes corporation or alliance gains or loses access to shared bookmark folders at once, but a shared folder the client already shows stays in its list until the pilot logs in again.
- Some runtime state lives only in memory and is lost when the server restarts:
  - fleets, with their finder adverts, member options and watchlists; combat and Crimewatch timers, ECM jams, tractor pulls, and in-progress hacking; a suspect flag from an activated kill right is the exception and is restored with its original deadline;
  - everything spawned into space except ships (NPCs, wrecks, scheduled effects);
  - drone orders and control, the drone settings the client sends before each launch, and ore a mining drone is carrying back;
  - a ship's velocity, warp and cloak, so a restarted ship starts at rest.
- A reload still running when the server stops is not resumed. At the next start its charges go back to the ship's cargo hold.
- A ship's position in space is saved when its pilot leaves the game, and every 30 seconds while it moves. A crash therefore loses at most the last 30 seconds of movement.
- A ship's shield charge and its armor and hull damage are saved when it docks, jumps through a gate or wormhole, or is swapped, when its pilot logs off, and with the periodic and shutdown position saves, including ships that are not moving. Damage to an unpiloted ship in space is saved as it happens. Armor and hull damage survive a restart, and shields recharge from the time they were saved. A crash can lose condition changes made since the last periodic save.
- Stopping the server with Ctrl-C or SIGTERM (`docker compose stop`) first stops starting scheduled jobs and gives the running ones up to `scheduler.drain_secs` to finish, then saves the position and condition of every ship in space before it exits. The server waits at most 10 seconds for these writes.
- Collisions use one sphere per ship or celestial, as the client does by default, not the rendered shape: a ship can bump into empty space near a station, or pass through parts of it that stick out of its sphere. Undocking uses a fixed heading and clearance rather than the station's undock points, so a very large ship can still start overlapping the station. Dynamic orientation, formations and destiny's rarer ball modes are not implemented.
- Ships have no orientation of their own. A micro jump or a bomb goes along the ship's velocity, or along its last commanded heading when it is stopped, or along a fixed default direction when it has neither.
- Balances are stored with up to 36 digits (ISK: 34 integer digits and 2 decimal places; PLEX: 36 integer digits), and the server works with the full value. The client is sent at most the largest balance it displays without scientific notation: 1,000,000,000,000,000,000 ISK for a character wallet, 9,223,372,036,854,774,784 ISK for a corporation wallet, and 99,999,999,999,999 PLEX. ISK reaches the client as a double, so above about 45 trillion ISK it can no longer show every cent.
- Injector diminishing returns are calculated per unit from the character's skill-point total, rounded down; one non-diminishing charge exempts one large or small injector, not a mini injector.
- A booster lasts its type's duration with the modifiers the static data defines. The current data defines no skill modifier for booster duration, so Biology does not lengthen boosters. A booster consumed before boosters had an expiry keeps an unknown expiry.
- A `/dogma` override changes every value the server computes. The client's fitting window and HUD show it only for an attribute the ship's hull type has; for a module, or a ship attribute the hull type lacks, they keep showing the type's value, while Show Info in space shows the new one. The command's reply says so.

## Repository layout

| Path | Contents |
|---|---|
| `crates/server` | The server (`infinity-server` binary) |
| `crates/server/lua` | Lua scripts the server loads at start (see [docs/lua.md](docs/lua.md)) |
| `crates/server/src/services` | Remote services the client calls (`sm.RemoteSvc(name).Method(...)`), one module per feature area |
| `crates/server/sql` | `game_schema.sql` (applied at every start) and `market_seed.sql` |
| `crates/server/retail` | Data captured from the retail server for this build (CDN content, cached machoNet calls) |
| `crates/marshal` | `blue.marshal` codec, plus the `marshal-dump` tool |
| `crates/sde-import` | Loads static data bundles (JSONL) into PostgreSQL |
| `crates/lp-import` | Operator tool that imports NPC corporations' LP store offers from ESI (see [docs/lp-import.md](docs/lp-import.md)) |
| `crates/blue-patch` | Patches the client's `blue.dll` so an edited `start.ini` still loads |
| `db/` | Per-table database dumps that are loaded into a fresh database |
| `scripts/` | Database init and dump, client patching and launching, and the dev loop |

## Requirements

- Docker with Compose.
- The Infinity client, build 3653293, on Windows.
- PowerShell, to run the helper scripts in `scripts/`.
- `crates/server/proto/eve_protos.desc`: the protobuf descriptor set extracted from the client.
  - The public gateway decodes the client's messages with it, and the server does not compile without it.
  - It is included in the repository. The extraction tool is not.
- Optional: a Rust toolchain, for local builds and tests. A local toolchain also lets `patch-client.ps1` build `blue-patch` itself instead of running it from the Docker image.

## Quick start

1. Start the database and the server:

   ```sh
   docker compose up -d --build
   ```

   On the first start with an empty database volume, `scripts/db-init.sh` loads every dump under `db/`. This takes a few minutes, and the server waits until the database is healthy.
2. Prepare the client. Close the client first, and run this again after every launcher update:

   ```powershell
   scripts/patch-client.ps1 -Root <client folder>
   ```

   The script does two things:
   - It patches `bin64/blue.dll` so the client skips its manifest entry hash check. The original is kept as `blue.dll.orig`.
   - It sets `cryptoPack = Placebo` in `start.ini`. The original is kept as `start.ini.orig`.

   Pass `-Check` to only report what is needed.

   `-Root` is remembered in `.client-root` (ignored by git), so later runs of both scripts can omit it. Without it, the scripts use `$env:INFINITY_CLIENT`, then the remembered folder, then `<drive>:\EVE\SharedCache\infinity` on any drive.
3. Launch the client against the server:

   ```powershell
   scripts/run-client.ps1 -User <name> -Password <password>
   ```

   The script does the following:
   - It disables TLS for chat in the client's `prefs.ini`.
   - It points the client at the public gateway.
   - It starts `portrait-sync.ps1`, which uploads portraits the client renders locally to the image server.

   Pass `-AutoSelect <slot or characterID>` to enter the game straight away. The default server address is `127.0.0.1`; pass `-ServerHost` to change it.

## Ports

| Port | Service |
|---|---|
| 26000 | machoNet (game client) |
| 5222 | Chat (XMPP, plain TCP) |
| 26001 | Image server (portraits, logos, type images; `PUT` stores a portrait) |
| 26002 | Public gateway (gRPC over plaintext HTTP/2) |
| 80 | Content CDN (the client always fetches it on port 80) |

## Configuration

The server reads `config.toml` from its working directory, or the file named by the `CONFIG` environment variable. Every key is optional: a missing file means the built-in defaults, and `config.example.toml` lists every key with its default and where the default comes from. The file is read once at start; changes need a restart. `docker-compose.yml` mounts `config.example.toml` as `/config.toml` and sets `DATABASE_URL`, `RUST_LOG` and `SYSTEM_CAPACITY`.

A deployment key can also be set by its environment variable, which wins over the file; the file wins over the default. Game keys have no environment variables.

The game sections, `[accounts]` through `[space]`, set the server's own rules: starting balances, fees and taxes, timers, limits, rewards, and spawn ranges and sizes. Their defaults are the values the server used before the file existed. Values the client checks itself, values from the static data, and the destiny physics constants cannot be changed. `characters.starting_isk` accepts 0 to 9,999,999,999,999.99 (an amount with more than 15 significant digits would not be stored exactly); `accounts.starting_plex` accepts 0 to 9,223,372,036,854,775,807.

Start-up stops with an error that names the file and the dotted key when a key is unknown or a value is invalid, including an invalid environment variable. Errors show numbers and `true`/`false` values only; strings such as passwords and connection URLs are never printed. The `configuration loaded` log line names the file (or `defaults`) and the database host, port, user, database name and pool size in use.

| Key | Variable | Default | Meaning |
|---|---|---|---|
| `database.url` | `DATABASE_URL` | empty | PostgreSQL connection string or URL. When set, it wins over the five fields below. Keep passwords in the environment, not in the file. |
| `database.host`, `.port`, `.user`, `.password`, `.name` | `DATABASE_HOST`, `DATABASE_PORT`, `DATABASE_USER`, `DATABASE_PASSWORD`, `DATABASE_NAME` | `127.0.0.1`, `5432`, `infinity`, empty, `infinity` | Connection used when `database.url` is empty |
| `database.pool_size` | `DATABASE_POOL_SIZE` | `16` | Maximum number of PostgreSQL connections the server opens. Keep it below the server's `max_connections` (PostgreSQL's default is 100), leaving room for tools. |
| `database.application_name` | | `infinity-server` | Connection name shown in `pg_stat_activity` |
| `database.connect_timeout_secs` / `.checkout_timeout_secs` | | `5` / `30` | Time limit for one connection attempt, and for waiting for a free pooled connection (`0` = wait forever) |
| `database.startup_retry_secs` | | `[1, 16]` | First and longest wait between connection attempts at start; the wait doubles |
| `database.schema_on_start` | | `true` | Apply `game_schema.sql` at every start. With `false` the server skips it and logs a warning; the schema must then already exist. |
| `scheduler.poll_secs`, `.lease_secs`, `.max_concurrent` | | `5`, `60`, `8` | Scheduled jobs (market order expiry, auction settlement, bills): the longest wait before due jobs are picked up, how long a claimed job may run before another start may reclaim it (must exceed `poll_secs`), and how many run at once |
| `scheduler.retry_base_secs`, `.dead_letter_attempts`, `.drain_secs` | | `30`, `25`, `10` | A failed job is retried after `retry_base_secs`, doubling up to 256 times that; a one-off job that fails `dead_letter_attempts` times stops and is kept for `/jobs`. The periodic jobs are never stopped. `drain_secs` bounds the wait for running jobs at shutdown. |
| `server.public_host` | `PUBLIC_HOST` | `127.0.0.1` | Address clients use to reach this host (image server and CDN URLs). Set it when clients are not local. |
| `server.system_capacity` | `SYSTEM_CAPACITY` | `0` | Pilots in space per solar system before stargate jumps queue (`0` = no limit) |
| `server.bind` | `BIND` | `0.0.0.0:26000` | machoNet listener |
| `chat.bind` / `chat.domain` | `CHAT_BIND` / `CHAT_DOMAIN` | `0.0.0.0:5222` / `infinity` | XMPP listener and domain |
| `images.bind` / `.port` / `.dir` | `IMAGE_BIND` / `IMAGE_PORT` / `IMAGE_DIR` | `0.0.0.0:26001` / `26001` / `/data/images` | Image server listener, the port advertised to clients, and its storage |
| `images.upstream` | `IMAGE_UPSTREAM` | `https://images.evetech.net` | Where missing NPC, NPC corporation and type images are fetched from once (empty = never) |
| `gateway.bind` | `GATEWAY_BIND` | `0.0.0.0:26002` | Public gateway listener |
| `cdn.bind` | `CDN_BIND` | `0.0.0.0:80` | Content CDN listener |
| `server.macho_version`, `.boot_version`, `.boot_build`, `.boot_codename`, `.boot_region`, `.node_id` | `MACHO_VERSION`, `BOOT_VERSION`, `BOOT_BUILD`, `BOOT_CODENAME`, `BOOT_REGION`, `NODE_ID` | match build 3653293 | Values the client checks during the handshake |
| `logging.filter` | `RUST_LOG` | `info,infinity_server=debug` | Log filter |
| `lua.enabled` | | `true` | Load the Lua scripts. With `false` no Lua VM is created and no call is claimed by a script. |
| `lua.scripts_dir` | | `crates/server/lua` | Directory the scripts are loaded from, relative to the working directory |
| `lua.memory_limit_mb`, `.call_budget_ms`, `.call_budget_instructions`, `.instruction_hook_interval` | | `64`, `50`, `5000000`, `1000` | Lua VM memory, and the time and instructions one call may use (counted every `instruction_hook_interval` instructions). A call over budget fails; a game change it already started still completes. |
| `lua.store_value_limit_bytes` | | `65536` | Largest value a script may keep in its persistent store |
| `lua.scripts.<name>` | | none | Settings only the script `<name>` can read |

Some keys switch behaviour on or off rather than set a number:

| Key | Default | Effect |
|---|---|---|
| `development.auto_register_accounts` | `true` | Create an account the first time an unknown name logs in. With `false`, an unknown name is refused like a wrong password. |
| `development.auto_register_role` | `player` | Roles of accounts created that way: `player`, or `gm` for the GM roles as well |
| `server.login_takeover` | `true` | A login to an account that is already online replaces the old session. With `false` the new login is refused and the old session stays; the client shows its generic "not accepting connections" message, because it has no "already logged in" message. |
| `combat.bounty_multiplier` | `1.0` | Scales the ISK paid for NPC kills (`0` = none). Loot and security status gains are unaffected. |
| `repair.cost_multiplier` | `1.0` | Scales the ISK station repair charges per hitpoint; must be positive. A repair whose price rounds to less than one cent is refused rather than done for free. |
| `industry.job_time_multiplier` | `1.0` | Scales industry job durations (below 1 is faster). Materials and costs are unchanged; the duration the client shows before a job starts does not include it. |
| `crimewatch.concord_enabled` | `true` | With `false`, CONCORD does not respond. Criminal flags, timers and security status penalties still apply. |
| `bounty.kill_right_lifetime_days` / `.kill_right_suspect_secs` | `30` / `900` | How long an earned kill right lasts, and how long activating one makes the aggressor a suspect. Both must be positive. |
| `gm.spawn_distance_m` / `.clear_radius_m` / `.max_spawn_count` | `5000` / `100000` / `100` | Default distance of GM-spawned NPCs, debris and ships, default radius of `/npcclear`, and the most NPCs one GM command may spawn |
| `salvage.salvage_material_stacks` / `.salvage_quantity_min` / `.salvage_quantity_max` | `1` / `1` / `3` | What a successful salvage gives, since wreck yield tables do not ship with the client: this many stacks of a random published salvaged material, each of between the minimum and maximum units. All must be positive, and the maximum at least the minimum. |
| `mining.ledger_retention_days` / `.ledger_maintenance_secs` | `90` / `3600` | How many days of personal mining ledger history are kept (90 is the client's chart range), and how often older entries are deleted. Both must be positive. Deleted history does not come back if the retention is raised later. |
| `map.history_retention_hours` / `.maintenance_secs` | `48` / `3600` | How many hours of star-map jump and kill history are kept, and how often older events are deleted. The retention must be at least 24, the longest window the client asks for, and the interval positive. Visits are never deleted. |

## Database

The database has four schemas:

- `sde` holds the typed, indexed static data tables.
- `sde_native` and `sde_compat` hold the raw static data bundles, one `jsonb` table per file.
- `game` holds everything players create: accounts, characters, items, the market, mail and so on.

Lifecycle:

- **First start.** `scripts/db-init.sh` loads `db/<schema>/<table>.sql.gz` into an empty volume. Tables whose foreign keys aren't satisfied yet are retried until everything is loaded.
- **Every server start.** The server applies `crates/server/sql/game_schema.sql`, loads the captured retail prices, and seeds the NPC market if it is empty. It then reads the static data it uses from `sde`, `sde_native` and `sde_compat` into memory once; after startup the server never queries those schemas, so static data changes take effect at the next start. Scheduled jobs are kept in `game.jobs`: jobs that fell due while the server was down run right after the start, and a job left half-done by a crash runs again once its lease expires, so every job must be safe to repeat.
- **Reset.** Remove the database volume to start over (`docker compose down`, then `docker volume rm infinity_pgdata`). The next `docker compose up` imports the dumps again.
- **Rebuild the static data.** Put the static data JSONL bundles (`eve-online-client-static-data-<build>-jsonl` and `eve-online-static-data-<build>-jsonl`) under `./sde/`, run `docker compose run --rm sde-import`, then restart the server.
- **Refresh the dumps.** `scripts/db-dump.ps1` replaces the folders under `db/` with dumps of the running database.

## Development

- **Builds.** Only release builds are used: `cargo build --release`, `cargo test --release`, or `docker build --target test .` to run the tests in Docker. The toolchain version is pinned in `rust-toolchain.toml`.
- **CI.** Every pull request to `main` must pass the `CI` workflow before it can merge:
  - `test-linux`: build and tests on Linux.
  - `build-windows`: build on Windows.
  - Both treat compiler warnings as errors.
  - Both build only when the pull request changes code: `crates/`, the Cargo files, `rust-toolchain.toml` or the workflows. For a pull request that changes no code, they report as skipped and it can still merge.
- **Dev loop.** `scripts/iterate.ps1` does the following:
  - rebuilds and restarts the server container;
  - launches the client into the game;
  - waits (`-Seconds`, default 70);
  - prints the run's unhandled calls, server errors and client exceptions.

  `-NoBuild` skips the rebuild.
- **Tools.**
  - `marshal-dump [--check] <file>...` decodes `blue.marshal` files. `--check` re-encodes them and reports only the status.
  - `blue-patch [--check] <bin64/blue.dll>` is the patcher used by `patch-client.ps1`.
- **Contributing.** Contributor and coding-agent guidelines are in [AGENT.md](AGENT.md).

### GM commands

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
| `/lua list` | List the loaded scripts, the calls, commands and events they claim, and their limits |
| `/lua reload` | Load the scripts again. If any script fails to load, the old set stays in use; calls already running finish on the old set. |
| `/motd [set <text>]` | Show or set the message of the day (bundled `motd.lua` script) |
| `/help` | List the commands |

Arguments may be quoted to include spaces. Lists longer than 50 rows show the first 50 and the number left out.

## Releases

Tagged versions are published on the repository's GitHub releases page. Each release contains:

- `infinity-<version>-x86_64-linux.tar.gz`
- `infinity-<version>-x86_64-windows.zip`
- `SHA256SUMS`

Each package holds `infinity-server`, `sde-import`, `marshal-dump`, `blue-patch` and this README.

To release, a maintainer pushes a tag `vMAJOR.MINOR.PATCH` that points at a commit on `main`, for example `git tag v0.1.0 && git push origin v0.1.0`. Tags with any other name do not start a release.

## License

The workspace manifest declares the MIT license.
