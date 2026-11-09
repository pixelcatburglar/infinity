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

## Client static-data import and deployment

### Adaptation content publication

The independent publisher accepts the proven decoded seven-family
`cfsd-adaptation-dataset` JSON bundle, not raw fsdbinary. It requires all seven
families and fails if any are missing. Supply the matching client's
`shipAdaptionSystem/common/const.py` as constants input; its hash records
provenance, but matching the client build is the operator's responsibility.
The published `chip_types` catalogue names virtual balance currencies, not
physical injectors; `chip_type_symbols` records its source symbols. Injector
conversion uses the startup SDE's target and per-item quantity attributes.
Keep client-derived data and published output outside the repository.

Create the external root first, then run:

```sh
cargo run --release --locked -p sde-import --bin adaption-content -- \
  --input <decoded-bundle> --client-constants <constants-file> \
  --output <external-root> --publish
```

Omit `--publish` to build without updating the regular atomic `current` pointer.
Output is versioned by build and manifest SHA with datasets and manifest;
historical incomplete validation is retained as provenance, not a claim that
node or task gameplay is complete. The ordinary `sde-import` default binary
and its importer CLI remain unchanged.

Set `lua.content_data_dir` or `LUA_CONTENT_DATA_DIR` to the external root.
The loader checks schema, build, paths, hashes and references against the
startup SDE, and captures the bundle once per Lua generation; requests do not
read files. A configured missing or invalid package or consumer fails startup.
A failed reload retains the previous complete generation. Bump the authored
content version whenever policy, messages or registration changes; immutable
retained versions reject different bytes under the same version and hash.

`sde-import` is an offline operator tool. The server continues to load static
data from PostgreSQL into its startup SDE; it does not read client files at
runtime. This import covers the dependency-closed Types consumer projection:
types, groups, categories, metagroups, marketgroups, dogma attributes,
attribute categories, units, effects and type dogma, type materials, required
skills, blueprints, and main, en-us and zh FSD localization. It does not
establish gameplay, UI, adaptation or wreck readiness, or decode every field
of those resources.

For these sixteen families the client is authoritative, including where it
differs from the comparison snapshot; there are no Elysian field or record
supplements. Other, not-migrated families and languages still come from the
remaining Elysian bundle. Unknown nonconsumer bytes are attributed and
reported, not assigned invented values. Missing zh rows remain missing, with
warnings and no English or old-data fallback. A dangling blueprint reference
is a warning only if the identical snapshot key, activity, path and target
already had it; new dangling references fail validation.

All sixteen families have native destinations. Only eight also have compat
destinations used by typed SQL: types, groups, categories, typedogma,
typematerials, dogmaattributes, dogmaeffects and blueprints. The other eight
are native-only; no new compat tables are created for them. This is 24 raw
destinations in total. Old bundle jobs cannot write any of the sixteen
families in either schema; an absent compat destination is not a fallback.
Raw readback covers the sixteen native and eight compat destinations.

Supply a resource root containing `staticdata` and `localizationfsd`, including
matching main, en-us and zh pickle resources, along with the remaining bundle
and typed SQL projection. Discovery handles case-insensitive directory names
but rejects ambiguous matches. The operator must ensure the resources, bundle,
build and hotfix versions match; do not infer identity from a directory name.
Treat the external report directory as private and trusted; it contains
client-derived sidecars that must never be committed. The importer uses
PostgreSQL `NoTls`; use a trusted local connection or a separately secured
tunnel and avoid literal credentials in commands or configuration examples.

Blueprint input is staged as owned, hashed bytes in the run-private
`source.sqlite`. Its SHA-256 is verified before SQLite opens the copy
read-only and immutable; the decoder does not reopen the original. Source
identity and journal checks still apply. `blueprints-metadata.json` links the
original and private copy with their SHA-256 evidence.

The blueprint SQLite file URI requires Windows paths to be valid Unicode
(UTF-8). Windows drive and UNC paths are supported, while unsupported special
device namespaces are rejected. On Unix, path bytes are retained without
requiring UTF-8.

```sh
DATABASE_URL="$CANDIDATE_URL" sde-import \
  --sde "$SDE_ROOT" --client-dir "$CLIENT_RESOURCE_ROOT" \
  --typed-sql "$TYPED_SQL" --report-dir "$REPORT_DIR" --jobs 4
```

All arguments except `--jobs` are required. Its default is 4; a supplied zero
is treated as 1. Set the variables to deployment-specific paths and a
connection for the fresh candidate described below, never a live production
database. `DATABASE_URL` is required.

### Reports and acceptance

The report directory's `manifest.json` describes a run, with
`schema_version` 1, phase and status: `preflight-failed`, `import-failed`,
`readback-failed` or `success`. Family statuses are `enabled`, `not-migrated`
or `blocked`; readback statuses are `not-run`, `passed` or `failed`. The
manifest records run metadata, source paths, SHA-256 hashes, sizes, build
identity and provenance when known, family authority, supplements, counts,
validation, readback and report paths. An unknown build identity is not a
license to guess it: review the issue and independently establish that the
inputs match.
Failed preflight reports identify blocked target families and progress; an
empty family list is not evidence of a successful import.

Trust a manifest only when this invocation exited 0 and its run identity matches the identity printed at invocation start.
The startup identity is printed to stdout or stderr and corresponds to
`run_metadata.run_id` in the manifest. When the report root is writable, the
invocation first publishes a non-success start marker. If initial marker
storage fails, the invocation exits nonzero without connecting to the
database; any older manifest is historical, not evidence for this invocation.

Run-specific reports include:

- `differences.jsonl`: complete client-versus-bundle snapshot additions,
  removals and field differences, including numeric representation
  classifications. These are authority comparisons, not database readback.
- `issues.jsonl`: errors and warnings, including references, localization,
  ownership and input identity evidence.
- `unknown-regions.jsonl` and `ownership-summary.json`: source hashes,
  attribution and exact unknown bytes or hashed sidecars, plus byte totals.
- `readback-differences.jsonl`: native, compat and typed mismatches against
  independently prepared expectations, including missing or extra rows.
  Localization metadata sidecars retain source metadata.
- `typed-unmigrated-readback.json`: baseline and per-table evidence for
  unchanged typed tables; family reasons reference its run-relative path.

For known binary fields, source comparisons use exact IEEE values: an old
snapshot integer lexeme is exact decimal, while a fractional or exponent
lexeme is interpreted as binary64. JSONB may have lost a positive-exponent
token, so its provenance limits comparison to the stored value; there are no
ID-specific exceptions. Raw COPY/JSONB readback instead compares canonical
decimal values, independently of this source-comparison rule.
For en-us and zh source comparisons, old `languageID` provenance is validated
and then excluded from the comparison; the new native records retain the four
source fields without a fallback.

The typed SQL rebuilds the whole typed schema. Ten migrated typed tables are
checked against independent decoded expectations; every other recreated typed
table is checked against its pre-import baseline using complete row counts and
SHA-256 of ordered binary records, including floating-point bits. These checks
prove whether those tables are unchanged, not that their content came from the
client. A missing baseline fails before mutation: restore a working candidate
from the complete snapshot rather than importing into an empty database.

Preflight builds the typed expectations exactly once before any database
mutation and caches them for readback. It rejects values outside existing
`i32`, `i64` and `f64` target ranges, incompatible JSONB text or keys,
NUL characters, and values outside the actual blueprint activity consumer
domains, with family, key and field evidence. Existing optional DAO semantics
are preserved; missing raw values are not filled with defaults.

Presence flags distinguish an omitted value from explicit null; zero and false
are values, not absence. Inspect all report records, not just bounded console
examples. Exit 0 requires complete validation, import, typed projection, full
readback and durable success manifest; exit 1 means failure. Check the current
run's manifest and report files as well as the exit code. A failed candidate
is unusable: table imports use separate transactions, so failure can leave a
partially changed database. Do not deploy it or rely on a prior success report.

### Production maintenance sequence

The commands below are templates, not an automated promotion system. Replace
the variables with your deployment's values before use; use an external
authentication mechanism such as a protected PostgreSQL service file or
`PGPASSFILE`, not literal credentials. PostgreSQL client environment settings
and roles must reach the intended cluster and provide the necessary backup,
create and restore permissions. Use compatible PostgreSQL versions. A database
dump does not include cluster-global roles or tablespaces: preserve and
provision those separately as appropriate. Keep ownership and ACLs consistent
with the deployment; do not use `--no-owner` unless the chosen restore roles
and ownership policy explicitly require and accommodate it.

1. Block external writes and stop the server, scheduled jobs, importers and
   every other writer. Prove they are stopped and that no write-capable
   connections remain. Keep writes blocked throughout snapshot, import,
   smoke and promotion.
2. Take a **complete, latest post-stop production** custom-format backup,
   including game state and the latest writes. Use a protected external path
   that does not already contain another backup; retain it for rollback.
   With `PGHOST`, `PGPORT`, `PGUSER` and authentication configured for the
   intended cluster, replace these example values:

   ```sh
   PROD_DB='replace-production-database'
   CANDIDATE_DB='replace-fresh-candidate-database'
   ROLLBACK_DB='replace-fresh-rollback-database'
   POST_STOP_BACKUP='replace-with-absolute-new-backup-file'
   pg_dump -Fc --file="$POST_STOP_BACKUP" --dbname="$PROD_DB"
   ```

   Confirm the backup completed successfully and is readable before proceeding.
3. Create a **new**, unused candidate, and restore that exact snapshot, not an
   earlier clone. The command must fail if the candidate already exists; never
   overwrite an in-use database. Supply any deployment-specific create/restore
   options needed for locale, extensions, roles and permissions.

   ```sh
   createdb --template=template0 "$CANDIDATE_DB"
   pg_restore --exit-on-error --dbname="$CANDIDATE_DB" "$POST_STOP_BACKUP"
   ```

   Set `CANDIDATE_URL` to a connection to this candidate and set `SDE_ROOT`,
   `CLIENT_RESOURCE_ROOT`, `TYPED_SQL` and `REPORT_DIR`, then run the import
   command above. Accept only exit 0 and a current `success` manifest with
   complete passing readback. On any failure reject the candidate and restart
   from a fresh restore; do not repair a partial import in place.
4. Capture game-state snapshots and keysets before a controlled smoke against
   the candidate. Stop the smoke process and all its writers, then compare the
   post-smoke state and keysets with the pre-smoke evidence. Verify that no
   test characters, items or other unapproved mutations remain. Reject and
   recreate a contaminated candidate; never improvise deletion of production
   rows. A successful import alone is not smoke or gameplay evidence.
5. With writes still blocked, the maintainer chooses the existing deployment
   mechanism to point the server at the accepted candidate (for example, set
   the server's `DATABASE_URL` to the candidate connection in its protected
   deployment configuration). Start and check the intended services; unblock
   writes only after promotion succeeds. Retain the post-stop backup.

For rollback, block writes again and stop and verify all writers. If writes
were allowed after promotion, explicitly account for those later writes:
restoring the post-stop backup would discard them and requires an authorized
recovery decision. Restore the retained backup to a new rollback database,
then have the maintainer switch the existing endpoint/configuration to that
database and verify services before unblocking writes:

```sh
createdb --template=template0 "$ROLLBACK_DB"
pg_restore --exit-on-error --dbname="$ROLLBACK_DB" "$POST_STOP_BACKUP"
```

Use a separately configured rollback `DATABASE_URL` naming `ROLLBACK_DB`; do
not confuse it with the candidate connection. A static-only clone must never
replace production game state.

### Test-only backup rehearsal

Rehearse separately on a test candidate, never on production: stop and block
its writers, dump that candidate, restore to a fresh disposable database, start
and stop a controlled smoke, compare pre/post smoke game-state snapshots and
keysets, and record dump and restored database sizes. Drop the disposable
database and delete only its candidate-only dump in the same rehearsal step,
including failure cleanup. Use the same PostgreSQL roles and relevant
backup/restore options intended for the real operation. Never delete the
retained production post-stop backup as test cleanup, and never promote the
disposable rehearsal database.

## Ports

| Port | Service |
|---|---|
| 26000 | machoNet (game client) |
| 5222 | Chat (XMPP, plain TCP) |
| 26001 | Image server (portraits, logos, type images; `PUT` stores a portrait) |
| 26002 | Public gateway (gRPC over plaintext HTTP/2) |
| 80 | Content CDN (the client always fetches it on port 80) |
