# Configuration

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
| `logging.dir` | `LOG_DIR` | empty | Also write the server log to files in this directory (empty = stdout only); see Logs below |
| `logging.rotation` / `.keep_files` | | `daily` / `14` | When a new log file starts (`minutely`, `hourly`, `daily` or `never`), and how many files are kept, the current one included; at least 1, and ignored with `never` |
| `telemetry.client_error_log_mode` | | `B` | How the client reports repeats of an error it has uploaded once: `B` counts them, `D` also sends each player's count and the trace's `Arguments:` part. Applies to errors first seen after the change; see Client error reports below |
| `lua.enabled` | | `true` | Load the Lua scripts. With `false` no Lua VM is created and no call is claimed by a script. |
| `lua.scripts_dir` | | `crates/server/lua` | Directory the scripts are loaded from, relative to the working directory |
| `lua.mod_dirs` | | `[]` | More directories to load scripts from, each like `lua.scripts_dir`: its top-level scripts load in name order, and `require` finds modules only in the script's own directory. Two scripts with the same name stop the load |
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
| `fleets.reconnect_timeout_secs` | `120` | How long a member who disconnects or logs off keeps their fleet seat for the client's automatic rejoin. Must be between 1 and 120, the client's own reconnect limit. |
| `shared_settings.max_payload_bytes` / `.max_nesting_depth` | `262144` / `64` | The largest shared item-filter or fleet-broadcast link the server stores, in bytes of encoded text, and how deeply its data may nest. Both must be positive, and the depth at most 64. |

## Logs

The server always logs to stdout, which is what `docker compose logs` shows. With `logging.dir` set (for example `dir = "logs"`, or `LOG_DIR=logs`) it also writes the same lines, filtered the same way and without colour codes, to files in that directory, created at start; a directory it cannot create stops the start with an error. Files are named in UTC: `server.YYYY-MM-DD.log` daily, with `-HH` added hourly and `-HH-MM` added minutely, and `server.log` with `never`. Rotation is by time only: `keep_files` limits the number of files, not their size, and a `never` file grows without limit. A graceful stop writes out the last lines. In Docker, uncomment `LOG_DIR` and the logs volume in `docker-compose.yml` to keep the files on the host. Logs can contain player information: keep them private and out of commits (`logs/` is ignored by Git).

## Client error reports

The client uploads the traceback of each exception the server has not seen before, and afterwards only reports how often it happens again. The server keeps one row per stack in `game.client_errors`: the first traceback (its last 64 KiB), when it was first and last seen, and the number of occurrences. In `D` mode each player's repeats go to `game.client_error_reports`. The crash notes a client sends at its next login go to `game.client_crashes`. With `logging.dir` set, each new traceback is also written in full (its last 1 MiB) to `client-errors` files next to the server files, named, rotated and kept the same way (`client-errors.YYYY-MM-DD.log` daily), while the server log gets one line per new error. To switch one error between counts and details, set its `log_mode` to `B` or `D`; the client picks the change up at its next report, normally within 15 minutes. A report that is malformed or over a limit (1024 records per call, 64 KiB per argument or crash text) is refused whole, and repeats that name another account's player are skipped. The first traceback for a stack is kept, so a modified client could send a false one first; delete that error's report rows and then the error row, and the next client to hit it uploads it again. Client performance statistics and the client's HTTP bug reports are not collected. Tracebacks contain file paths, local variables and session state from the player's machine: keep the database and the log files private.

```sql
SELECT error_id, mode, log_mode, occurrences, last_seen, left(traceback, 200)
FROM game.client_errors ORDER BY occurrences DESC LIMIT 20;
```
