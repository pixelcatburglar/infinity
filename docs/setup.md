# Setup

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
