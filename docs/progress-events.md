# Server progress events

The server journals durable game events in the transaction that produces them.
Each transaction invocation has a source namespace and ordinal; a retry reuses
that namespace, while a duplicate source does not send another Lua event or
create more consumer receipts. Journal rows and receipts are not retained for a
limited period or deleted automatically.

Durable event schema 2 stores `WalletChanged` balance and delta as canonical
fixed two-decimal strings, preserving exact cents rather than client-capped
values. Lua receives uncapped numbers instead; large numbers can lose precision.
On startup, before consumers or listeners start, a one-time transaction promotes
schema 1 events to schema 2. It preserves event IDs, metadata and delivery
receipts, so previously acknowledged deliveries are not redelivered. Legacy
wallet numbers are converted from their recorded values; precision already lost
or clamped before recording cannot be recovered. Invalid legacy events reject
the migration and abort startup rather than being silently changed.

Inventory and market event quantities are signed 64-bit integers in newly
recorded events. Historical integer quantities remain readable without changing
event identity or delivery receipts.

Consumers register a stable key and event kinds before recovery starts. Startup
fails if an enabled subscription has no installed handler. New registrations
receive only future events. Each delivery is applied and acknowledged in the
same transaction; failures remain pending with bounded exponential retry. The
scheduler polls pending receipts after restart at its configured polling interval.
Consumer handlers must use their transaction and outbox rather than independently
committing changes.

The legacy `Targeted`, `Damaged`, `ModuleCycleEnded` and `WarpArrived` events
are live-only: they do not write journal or receipt rows. They are distinct
from the canonical combat facts below. Lua observes newly committed events
on a best-effort process-local channel; replaying durable deliveries does not
replay Lua callbacks. The ordering of Lua events within a transaction and the
ordering of client notifications are preserved independently; their relative
interleaving is not guaranteed.

Every durable event is produced inside a transaction; none reaches Lua or a
consumer without being journaled. `SkillStateChanged { char_id }` records a
trained skill change from free skill point application, extraction or GM
operations. It is a state invalidation, not a `SkillTrained` completion;
unallocated skill points and expert system changes do not emit it. The durable
personal milestone consumer handles both kinds by recomputing the viewer's
snapshot from current state rather than replaying event levels.

Resource facts carry an occurrence FILETIME and a historical context with optional
corporation, alliance, faction, ship, ship type, system and station identifiers,
plus the server-supplied origin (`natural`, `gm`, or `qa`). Unknown values are
null. Mining facts carry separately attributable source slices; the envelope's
occurrence is the earliest slice, not the time of every delivery in that batch.
Consumers must filter each slice before summing its quantity or total volume.

## Combat facts

`DamageApplied` and `RemoteRepairApplied` aggregate accepted, actual HP changes
by their full source, target, layer and historical context key. Damage is
post-resistance and excludes overkill; remote repair records the capped amount
actually restored, not the requested amount. Local and passive repair, GM health
changes, misses and zero changes are not ordinary combat facts. Each key keeps
an exact sum of the accepted binary HP amounts and a count, not per-hit records.
An eligibility or identity boundary closes the affected sub-slice before the
change takes effect; consumers filter sub-slices rather than apportioning a sum
across a boundary. Origin and context are captured historically, and unknown
dimensions remain null rather than being inferred from current state.
UTC boundaries are observed on existing actor ticks and checkpoints. If the
actor next observes a change from one day to three days later, it closes one
boundary at that observation; it creates no intermediate receipts or daily
segments. HP and counts remain attributed to their actual occurrence times.
Continuous intervals retain their true endpoints, and consumers attribute
duration by intersecting those intervals with UTC periods.

Source replay receipts cover live pilots and their owned native sources.
Departure or retirement reclaims them at a rare boundary or checkpoint;
a pending transferred source retains its metadata. There is no separate
4,096-receipt cap.

`WarpScrambleApplied` records successful projection upserts and refreshes as
cycles, and records actual activation intervals. A continuous refresh does not
duplicate duration; an end, expiry or target loss credits only the actual
elapsed prefix. An intervals-only fact may have a zero count, but a fact with
neither count nor intervals is rejected.

The shared capacity is 4,096 distinct aggregate keys, not occurrences. When
full, new scramble upserts and refreshes are refused even if they would merge
into an existing key; refused cycles do not increase the count. Active
intervals reserve their end, which cannot be refused. If a checkpoint or
qualification boundary cannot reserve a continuation, the projection is
removed in the same actor turn and no future duration is credited.

`NpcDestroyed` records a canonical NPC death, including an actual credited
killer when known. A pending NPC death commits its facts before the NPC is
removed; mining and canonical NPC death facts are committed with their
authoritative source transactions, so a failed transaction cannot lose them.
NPC rewards and wrecks retain their existing separate policy; the death fact
does not guarantee exactly-once rewards or wreck creation.

`LossSettled` records the committed player loss and frozen participants;
`PlayerKillCredited` records positive participant contributions. The final blow
comes from the lethal source, not necessarily the largest contributor. The loss
valuation snapshots the current retail average price as an exact binary value:
an unknown price is null, distinct from a known zero price. It has no price
source revision. NPC insurance paid is the actual posted amount in exact cents,
encoded in JSON as a canonical fixed-two-decimal value. Lua uses the existing
uncapped wallet number projection, which can lose precision for large amounts.
Destroyed implants are currently empty because actual body loss is not modelled;
active implants are not presumed destroyed. These facts do not
by themselves implement a progress interface or reimbursement policy.

Ordinary damage and remote-repair aggregates use the existing position and
condition checkpoint and its `space.position_snapshot_secs` interval (30 seconds
by default), not a separate fact timer or an early flush. They are journaled in
the same checkpoint transaction through the existing event journal and receipts;
there is no second inbox. A sealed batch retains its source key for replay after
failure or an ambiguous commit and is acknowledged only after COMMIT.
Space boarding, storing a vessel and boarding a stored ship use the same fenced
transition worker to commit the original fact frame with hull condition and
selection changes; recovery checks the frame's receipts before acknowledging it.
Daily-goal, achievement and project combat progress can therefore appear up to
one checkpoint interval plus normal delivery later. A hard kill can lose one
checkpoint window and an in-flight batch. During a database outage the configured
aggregate buffer is bounded: when full, new damage and remote-repair effective
cycles do not take effect until recovery frees capacity, rather than dropping
facts or queuing without bound. A hard kill during an outage may lose buffered
facts, with no fixed one-second loss bound.

## Resource facts

Mining delivery snapshots are committed with cargo and asteroid changes in the
personal ledger. New rows remain pending until aggregated; retention never deletes
pending rows, even beyond the personal ledger window. Existing ledger history is
considered already aggregated and is not attributed using current membership.
Corporation and alliance are captured at delivery, ship type is captured from the
source item, and faction is unknown. Ore and ice share the ore kind; gas has its
own kind. Drone ore cached before delivery is not yet a committed source fact.

The durable scheduler processes at most `mining.progress_batch_rows` pending rows
per invocation, at `mining.progress_aggregation_secs` intervals. It locks rows in
log-id order, groups only identical frozen context, kind and type, and commits
the marker and journal events together. A failed transaction leaves its rows
pending for retry; restarting seeds or recovers the same periodic job. A sustained
backlog needs sufficient batch capacity and is never discarded by retention.

Manufacturing installation is emitted after a manufacturing job is installed;
delivery is emitted after its guarded delivery and output deposit. Their business
keys use the job ID and distinguish installation from delivery. Research, copying,
invention, cancellation and readiness do not emit manufacturing facts.

Signature resolution is journaled only for the first durable character/site
authorization of an actual signature. Probe results below full certainty and
anomalies do not count. GM and QA resolutions carry their explicit origins.
Container hacks are journaled with the durable first claim and contents transfer;
empty containers can be claimed, while repeated claims emit nothing. Ordinary
site saves preserve claimed containers. Signature dungeon types use the durable
site's SDE dungeon definition ID when available; unknown dungeon types and
character factions remain null rather than being inferred.

Wreck salvage is journaled once per successful durable wreck claim, in the same
transaction as loot preservation and salvage deposit. Failed cycles, full cargo
and repeated claims produce no fact. World removal happens after commit.
Ordinary loyalty earnings are positive natural mission rewards, journaled with
the guarded mission completion and issuer-keyed balance update. GM adjustments,
store purchases, corporation spending and Heraldry are not ordinary earnings;
EverMarks grants are not implemented here. Organization and source ship type
are read during the source transaction; unavailable dimensions remain null.
