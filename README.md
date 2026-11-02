# Infinity

A server emulator for the EVE Online **Infinity** client (build 3653293, machoNet 496), written in Rust.

A single process acts as both proxy and sol node. It also serves XMPP chat, the image server, the public gRPC gateway and the content CDN. Game state is stored in PostgreSQL.

> This project is not affiliated with or endorsed by CCP Games. EVE Online is a trademark of CCP hf.

## Features

Pilots log in, create characters and play in stations and in space: items and direct trades, fitting with server-side dogma, flight and combat, skills, implants and boosters, the market, contracts and industry, agents and missions, exploration, the star map, mining, chat, fleets, mail, corporations and alliances, and the New Eden Store. Operators can extend the server with Lua scripts, and game masters have slash commands.

[Features](docs/features.md) describes each area, and [Known limitations](docs/limitations.md) lists what the server does not support.

## Quick start

You need Docker with Compose, and the Infinity client (build 3653293) on Windows with PowerShell.

```sh
docker compose up -d --build
```

```powershell
scripts/patch-client.ps1 -Root <client folder>
scripts/run-client.ps1 -User <name> -Password <password>
```

The first command starts the database and the server. The second prepares the client; run it again after every launcher update. The third launches the client against the server. [Setup](docs/setup.md) covers the requirements, what each script changes, and the ports.

## Documentation

| Document | Contents |
|---|---|
| [Setup](docs/setup.md) | Requirements, quick start and ports |
| [Features](docs/features.md) | What the server implements, area by area |
| [Known limitations](docs/limitations.md) | What the server does not support |
| [Configuration](docs/configuration.md) | Settings and their environment variables, log files and client error reports |
| [Database](docs/database.md) | Schemas, lifecycle, reset, static data rebuilds and dumps |
| [GM commands](docs/gm-commands.md) | Slash commands, and how to grant GM roles |
| [Lua extensions](docs/lua.md) | Scripts the server loads, and the script API |
| [Effects](docs/effects.md) | The effect system: registry, activation, fitting, heat and the fit evaluator |
| [Remote service coverage](docs/remote-services.md) | Client calls the server answers specially |
| [LP offer operator import](docs/lp-import.md) | Importing LP store offers with `lp-import` |
| [Development](docs/development.md) | Builds, CI, the dev loop, tools, repository layout and releases |
| [AGENT.md](AGENT.md) | Rules for every change to this repository |

## License

The workspace manifest declares the MIT license.
