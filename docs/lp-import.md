# LP offer operator import

`lp-import` is an operator tool, not a server-side ESI client. Prepare the
operator database with the typed SDE and the server's `game.lp_offers` schema
before running it. Give the database role read access to
`sde.crp_npc_corporations` and `sde.inv_types` and write access to
`game.lp_offers`. The PostgreSQL connection currently uses `NoTls`; use a
trusted local connection or a separately secured tunnel. Command-line database
URLs can be visible to other local users; use an appropriate local authentication
mechanism rather than embedding a password in the command line.

Example (replace the placeholders with operator settings):

```sh
cargo run --release -p lp-import -- --database-url '<postgres-url>'
```

By default the tool uses `https://esi.evetech.net/latest`; `--esi-base-url`
overrides the ESI version root for a compatible test service. Non-deleted SDE
NPC corporations with a non-empty `lpOfferTables` array are queried at
`GET /loyalty/stores/{corporation_id}/offers/`. SDE data determines eligibility,
not a hardcoded list. The importer requires all IDs to exist in the installed
SDE, checks positive quantities and nonnegative integer costs, preserves the
order of required items, and rejects duplicate offer IDs. An ESI/SDE version
mismatch can therefore reject an entire corporation; reconcile the data versions
before retrying. Failures are reported per corporation and the exit status is
nonzero if any corporation failed. The summary counts successful corporations,
committed offers and errors; each failed corporation contributes one error.

The response body limit defaults to 16 MiB per corporation, a server design
choice; operators can set a positive byte limit with `--max-response-bytes`.
Declared or streamed bodies exceeding the limit fail that corporation before
any of its offers are written. Increase the limit if a trusted ESI response
legitimately exceeds it; larger limits permit greater memory use.

The [ESI OpenAPI schema](https://esi.evetech.net/meta/openapi.json), schema
`LoyaltyStoresCorporationIdOffersGet`, makes `ak_cost` optional. Its absence
means zero; an explicitly present value must be a nonnegative integer.

Each complete, validated response is upserted in one transaction. A failure
leaves that corporation's prior rows unchanged; other corporations can succeed.
Repeating the import is safe. Upsert deliberately does not remove older offers
that are no longer present in a response; catalogue retirement requires a
separate operator decision. Run when the server's catalogue is not being
actively changed by another importer. The server reads `game.lp_offers` into
memory once at startup and never reloads it while running; restart the server
for an imported or adjusted catalogue to take effect.

Focused tests run with `cargo test --release -p lp-import`. The ignored
database test uses the real upsert transaction against a disposable empty
PostgreSQL database. It creates and removes a `game` schema, refusing to start
if one already exists; if interrupted, discard the database. Never point it at
an existing game database. Run it explicitly with:

```sh
LP_IMPORT_TEST_DATABASE_URL='<disposable-postgres-url>' cargo test --release -p lp-import database_upsert_repeatability_and_rollback -- --ignored
```

ESI is operated by CCP Games; its responses are EVE Online game data, not this
project's MIT-licensed code. Consult the current [EVE Developer site](https://developers.eveonline.com/)
and its applicable API and EVE third-party terms before obtaining, storing or
redistributing the data. The SDE is an external CCP data source as packaged by
the operator. No ESI payload or SDE-derived catalogue is included in this crate.
