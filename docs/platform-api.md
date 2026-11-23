# Platform API routing, observations and managed keys

Platform Admin credentials are separate from Codex OAuth. Only a local server
operator or standalone owner can provision them; the SSH gateway, GUI and TUI
never receive secrets or provide credential-management mutations.

## Configure

```sh
exrd admin oauth platform set-admin-key 1 --key-file /private/path/admin-input
exrd admin oauth platform configure 1 --project-id proj_example --manage-keys true
exrd admin oauth platform refresh 1
exrd admin oauth platform status 1 --json
exrd admin oauth platform models 1 --json
```

The project must already exist and belong to the Admin Key's organization.
Choosing it explicitly avoids silently modifying an unrelated project. Set
`--manage-keys false` for costs-only observation. The standalone owner uses
`exr account platform` with the same subcommands, arguments and implementation.
`refresh` can provision a working key if management was explicitly configured;
it never submits inference. Ordinary Doctor/history reads are passive.

Both runtime modes run one bounded Platform maintenance loop. Metadata refreshes
are coalesced durably and attempted at most once per 15-minute refresh interval;
an explicit refresh may run after the one-minute in-flight exclusion. HTTP
requests have ten-second deadlines, bounded bodies/pagination, no redirects and
a fixed official API origin. The entire refresh has a 40-second deadline.
Shutdown cancels work rather than leaving detached mutations.
Key maintenance requires a successful costs read; unavailable costs do not
trigger provisioning as a side effect of a failed refresh.

## Costs, not free-credit balances

The [organization Costs API](https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/usage/methods/costs)
provides **daily USD costs across the organization**, including activity outside
ExetRouter. It does not report verified remaining subscription credits.

Only numeric daily observations are retained, with exact integer nanodollars,
observation time and interval. Empty returned buckets confirm zero; missing
buckets, unsupported precision/currency and failed reads never imply zero.
Duplicate buckets or malformed pages cannot partially overwrite a refresh.
History retains at most 366 UTC daily buckets per configured account, even after
a backward clock change. The refresh rereads 90 days to accept provider revisions.
Admin credential generations prevent old or differently scoped refreshes from
publishing. Credential replacement hides old-generation data until it is observed
again. Removal deletes cost/working-key state, not upstream resources.

The current spending summary uses the configured expected billing day at UTC
midnight, clamped to the last day of short months; without a billing day it uses
the UTC calendar month. This is an explicit **reporting convention**, not a
verified invoice, grant, renewal instant or grant expiry. A contiguous observation
through the requested end is required for a total. Otherwise it stays unknown.
Observed spending against the configured budget is shown as `$10 / $100`, not a
claimed free-credit balance. Human dollar displays use `$5k`, `$12.5k`, `$1.2M`;
automation JSON retains numeric amounts. Tiny nonzero prices are not rendered $0.

All available Platform funds may be used, whether granted or purchased. There is
no free-only gate and the configured allowance is a display reference, not a local
spending cap. Upstream remains authoritative about available funds and balance
exhaustion. Daily costs are not subtracted from an assumed grant to admit work.

The operator can inspect organization-wide costs separately. Account details in
the TUI and Desktop instead show [estimated Platform percent remaining](quota-history.md)
on the same chart as Codex. Disconnected accounts have no Platform meter or line.

## Manual remaining-funds estimates

The Admin API does not provide a documented remaining-credit balance endpoint.
An operator can enter the amount currently visible in Platform Billing:

```sh
exrd admin oauth platform set-balance 1 --usd 75.25
exrd admin oauth platform status 1 --json
# Standalone owner:
exr account platform set-balance 1 --usd 75.25
```

Each entry **replaces** the previous reconciliation point and restarts both
estimates. It does not add credits or change the plan reference. There is no
automatic monthly refill: enter the new actual amount after renewal, top-ups,
expiry or other balance changes. Zero is valid. The operator/standalone-owner
commands are local only; the restricted gateway and GUI cannot edit balances.

The Overview meter defaults to **ExetRouter estimated remaining funds**: the
entered amount minus standard token-cost estimates for Platform requests admitted
after the entry. Native Codex requests and other accounts are excluded. The entry
atomically captures a durable usage-event boundary; requests already admitted
before it remain excluded even if they finish later. Token estimates are exact
integer nanodollars at the implementation's reviewed standard prices, including
cached input and the existing long-context tier. Each new finalized estimate is
frozen atomically with usage, not repriced on dashboard reads. This is not an
invoice: external activity, tool/storage fees and service-tier adjustments are
excluded. When some requests are unpriced (for example, after an interruption),
the dashboard and human-readable CLI show a **partial estimate**: the entered
amount minus known costs, with the number of excluded requests. The figure may
overstate remaining funds. Their costs remain unknown in accounting, and the
complete `remaining_usd_nanos` stays null in JSON; `spent_usd_nanos` contains only
the known subtotal. Negative estimates are not clamped or used to stop routing.

The operator's Platform status report additionally shows **Platform organization
estimated remaining funds**. An independent Costs API query starts at the manual-entry
timestamp, not at midnight or the subscription billing day. Its returned partial
boundary buckets are never prorated, and their amounts are not added to the
ExetRouter estimate. Missing, overlapping or duplicate intervals cannot establish
a total; a report with contiguous coverage is labeled with its actual through
time and observation time. Reporting can lag and be revised. Before a usable
report arrives, this alternative is unknown, not zero spend. The bounded query
supports up to 366 days since entry; older entries require a new reconciliation
point for organization reporting.

Both variants are estimates anchored to a manual value, **not a live provider
balance**. Credential/project configuration changes invalidate the reconciliation
point until re-entered. Epoch and credential-generation checks prevent delayed
reads from updating a replacement entry. Read-only status is shared identically
by remote/standalone CLI, GUI and TUI. No web-session credentials are required.

## Thirty-day working keys

The selected project uses the uniquely named `exetrouter` service account and
an API key named `exetrouter`, with a requested lifetime of exactly 30 days and
model-list, Responses and Embeddings scopes. An existing restricted working key needs the
Embeddings Request permission to use `/v1/embeddings`; upgrading the router does not
change or rotate that key. Enable the permission in the project's key settings or
wait for normal managed renewal. A missing service account is created with its
default `member` membership, without a separate RBAC role assignment. Its initial
bootstrap key requests the minimum supported one-second lifetime. The returned
secret is encrypted and committed atomically with creation confirmation, marked
inactive, and never used for model discovery or inference. Named-key provisioning
waits until that bootstrap expiry has elapsed; if necessary a later refresh
continues without creating another service account. A missing or unbounded
bootstrap expiry is an uncertain operation, not permission to try again.
Provider timestamps have second precision: an already-expired initial key may
report equal creation and expiry timestamps. This zero observed lifetime is valid
for a non-routable bootstrap key, never for a working key.

Existing role-less accounts require explicit operator reconciliation: management
does not automatically assign roles, delete or recreate service accounts. Existing
member/owner roles are not demoted. Duplicate names and archived projects stop
management. Bootstrap state uses the existing encrypted key row and confirmed
creation operation; no database migration or new permission is required.

Before reusing a stored secret, check the project's key inventory, owner access,
account binding and provider expiry; perform a non-generating model-list read.
Expired or confirmed-absent managed keys can be replaced. A live named key whose
one-time secret is absent locally is not silently deleted or duplicated. A revoked
key cannot be reactivated by recovering its redacted list value.

Every potentially mutating POST is durably recorded before submission. Key
ciphertext and successful creation confirmation commit atomically. Interruption,
malformed replies, unconfirmed expiry or a storage failure leave an uncertain
operation fence; **no automatic retry** occurs after restart or Admin Key
removal/replacement. The operator must reconcile uncertain operations upstream;
there is deliberately no blind retry/clear command. Deactivation prevents new
key-management mutations. Encryption authenticates account, project, key ID and
a separate versioned domain. Verified backups authenticate working keys too.

Reviewed public contracts: [service accounts](https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/projects/subresources/service_accounts),
[default membership and expiring initial keys](https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/projects/subresources/service_accounts/methods/create),
[create named expiring keys](https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/projects/subresources/service_accounts/subresources/api_keys/methods/create),
[project key inventory](https://developers.openai.com/api/reference/resources/organization/subresources/projects/subresources/api_keys/methods/list).
Offline fixtures verify these contracts, not a live account's permissions.

## Read-only model availability

`platform models ID` uses the actual stored working key, never the Admin Key. It
requires matching current credential/project generations, an active account and
operator availability, a nonexpired key and no uncertain key-management operation.
The same identity is rechecked after the read, so replaced credentials cannot
publish an older observation. It does not create, renew or activate a key.

The bounded, atomic inventory contains at most 2,048 unique model IDs with normalized
display names. Owner names, arbitrary metadata and payloads are discarded. The
public Models API establishes availability only, not endpoint capabilities or a
cash balance; no capability is guessed from a model name. The operator inventory
retains all observed IDs. Only the intersection with reviewed public Responses and [embedding](https://developers.openai.com/api/reference/resources/embeddings)
descriptors enters routing; JSON `routing_enabled` indicates whether that
intersection is nonempty, not whether a particular request has available funds.

The reviewed descriptors are `gpt-6.1-sol`, `gpt-6.1-terra`, `gpt-6-astra`,
`gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` and `gpt-5.5`:
Responses, function calling, text/image input, 1,050,000-token
context and 128,000-token output. Public model pages were reviewed November 13,
2026 ([Sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol),
[Astra](https://developers.openai.com/api/docs/models/gpt-6-astra),
[5.6 Sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol),
[5.6 Terra](https://developers.openai.com/api/docs/models/gpt-5.6-terra),
[6.1 Terra](https://developers.openai.com/api/docs/models/gpt-6.1-terra),
[6 Sol](https://developers.openai.com/api/docs/models/gpt-6-sol),
[6 Luna](https://developers.openai.com/api/docs/models/gpt-6-luna),
[5.6 Luna](https://developers.openai.com/api/docs/models/gpt-5.6-luna),
[5.5](https://developers.openai.com/api/docs/models/gpt-5.5)).
GPT-5.5 supports reasoning through `xhigh`, not `max`.
Additional streaming Responses IDs reviewed against the [public model pages](https://developers.openai.com/api/docs/models)
on November 23, 2026: `gpt-6.2-astra`, `gpt-6.2-sol`, `gpt-5.4`, `gpt-5.4-pro`,
`gpt-5.4-mini`, `gpt-5.2`, `gpt-5.2-pro` and `chat-latest`. Limits and reasoning
settings are model-specific. A Responses listing alone does not establish streaming
support; for example, the GPT-5.5 Pro page does not list streaming.
Unknown inventory entries remain inspection-only until their endpoint contract
is reviewed. No inference probe is used. Availability caches last 15 minutes.
Distinct eligible Platform IDs follow Codex models; shared IDs retain their
Codex descriptor and are marked as Platform fallbacks when the policy permits it.

A working key is marked active only after its model-list read authenticates and
returns a valid inventory. A failed read does not cause another key to be created;
the confirmed one-time secret and pre-POST fence are preserved for reconciliation.

## Select routing resources

The server operator can change the enabled resources for all users immediately:

```sh
exrd admin oauth platform resources codex
exrd admin oauth platform resources platform
exrd admin oauth platform resources codex platform
exrd admin oauth platform resources --json  # Show the current policy.
```

The standalone equivalent is `exr account platform resources`, with the same
arguments. With both resources enabled, argument order does not change routing:
Codex comes first for shared models, and Platform-only models route directly.
In a single-resource mode the other resource is never used as a fallback. The
model catalog reflects the enabled resources; embeddings require Platform, and
native images/search (including MCP tools) require Codex. Platform routes still
require a configured working key and an active, operator-enabled parent account.

The change is durable and applies to new admissions, including new frames on
existing WebSockets. It does not cancel in-flight generations, deactivate accounts,
delete credentials, or implicitly move saved conversations or opaque context.
If context cannot safely transfer, reconnect or resend self-contained text/tool
history. Disabling Codex also prevents reset-credit redemption and the weekly
activation probe. Read-only diagnostics and configured Platform key maintenance
are separate from inference routing.

To apply an explicit selection on every server startup, use
`"routing_resources": ["codex", "platform"]` in server JSON or
`exrd serve --routing-resources codex,platform`. The flag overrides the file.
Omit both to retain the most recent operator selection across restarts.
Before any explicit selection, the previous behavior is preserved: legacy
`true` acts as `codex platform`; the legacy default/`false` allows Platform-only
models and embeddings but not shared models on Platform. The old server setting
is read only for that inherited policy. Use `resources codex` to disable all
Platform inference, including embeddings.

The
[subscription credit benefit](https://developers.openai.com/api/docs/guides/chatgpt-api-credits)
may continue onto purchased credits, and [hard spend limits](https://developers.openai.com/api/docs/guides/spend-limits)
have delayed enforcement. Neither a free-only guarantee nor a strict local dollar
ceiling is claimed.

## Transport and context ownership

The shared backend implements Platform Responses HTTP JSON/SSE, translated Chat,
and HTTP-backed client WebSockets. Only the encrypted project working key is used
at the fixed `https://api.openai.com/v1` origin; Admin Keys, Codex account/turn
headers and the caller's bearer are never used for inference. The inference read
budget is 900 seconds, with redirects disabled and no router input-size ceiling.
Platform [compaction](https://developers.openai.com/api/reference/resources/responses/methods/compact)
uses the public unary `/responses/compact` operation, not Codex's SSE trigger.
Its returned visible prefix and opaque checkpoint are both preserved.

With both resources enabled, Codex remains first choice. Same-model Platform
fallback requires exhaustion of **every eligible Codex subscription for that model**.
Unknown quota, authentication failures, expired credentials or operational backoff
do not authorize API spending as a substitute. A recognized pre-generation quota
refusal can trigger the existing bounded fallback; uncertain submitted inference
is never replayed. If no safe Platform replacement is available, preserve the
native credit-backed choice or original refusal.

Each account/project has a distinct durable route identity, separate from OAuth.
Working-key rotation preserves that project's context identity but fences old
credential observations. Platform failures/cooldowns never modify the parent's
Codex quotas or OAuth refresh state. Refreshing/rotating a working key does not
erase an outstanding project quota rejection. Durable usage records retain the parent
account and actual `platform_http_sse`/`platform_http_json` transport, including
unknown outcomes on interruption/restart.

Opaque reasoning, checkpoints and saved conversations remain user- and
project-owned. Codex receipts/legacy registry overrides cannot authorize Platform
state. Cross-backend or cross-project opaque transfer is not assumed portable:
fallback requires self-contained text/tool context, without project-owned file or
container references. No checkpoint or reasoning history is silently discarded.
Same-project HTTP continuations and bounded WS recovery preserve exact input;
unrecoverable incremental WS requests fail before accounting/submission and need
client-supplied full input. Public HTTP cannot emulate `generate=false` native
warmup: it returns a local unsupported-parameter error without inference or a
usage row, leaving the client WS open for a generating request.

These adapters have synthetic upstream coverage, not a renewed live/native-client
compatibility matrix. Real permissions, funds and model access remain provider
decisions. Standalone and remote servers use the same implementation.

Passive Doctor/Overview readiness includes eligible observed Platform routes and
the combined catalog, without decrypting credentials or contacting upstream.
Native pool/quota counters remain Codex-only; API routes are not fabricated
subscription accounts. A healthy API route can therefore keep the router ready
while every native subscription is in quota cooldown.

## Release upgrade from schema 15

The 0.10.0 server/standalone uses schema 20; the 0.9.2 compatibility release uses
schema 15. This is an explicitly reviewed migration, not a routine Docker updater
operation. Publishing the release never changes a deployed database.

1. Inspect the actual deployment, preserve its configuration, units, ingress and
   restricted SSH boundary, and retain the matching old server/gateway binaries
   or image. Stop/drain all writers and create/verify a private consistent backup
   with the existing token-signing and OAuth/Platform credential keys. Rehearse
   restore and migration on an isolated copy with no running inference service.
2. Review the changes in order: [15 → 16 history rebucketing](quota-history.md#schema-15-to-16-ten-minute-buckets),
   [16 → 17 subscription metadata](subscription-metadata.md#migration-and-rollback),
   [17 → 18 Platform observations](#schema-18-upgrade-and-rollback),
   [18 → 19 route identities](#schema-19-upgrade-and-rollback), and
   [19 → 20 manual balances](#schema-20-upgrade-and-rollback). Startup applies
   pending migrations transactionally. Allow space for the temporary history copy.
3. Deploy matching schema-20 server and native gateway. Verify `user_version=20`,
   database integrity/foreign keys, existing authentication and account/usage
   retention, health and private permissions with passive reads. No Admin Key,
   project, balance or working key is fabricated by migration. Provisioning still
   requires explicit operator configuration; do not start a second live router
   using the same copied credentials for rehearsal.
4. Update remote native clients to 0.10.0 and Desktop plugin to 0.5.0 together.
   Reopen existing dashboards after replacement. The remote client does not
   migrate the shared server. For an already schema-20 deployment, preserve all
   current manual anchors, frozen costs, route IDs, encrypted credentials and
   uncertain-operation fences; this release does not require a new schema step.

Rollback must use a verified **current** snapshot, not overwrite newer usage with
the pre-upgrade backup. Compose the linked rollback procedures in reverse order.
Retain the additive Platform tables, epochs, operation fences and route sequence.
Returning to schema 15 additionally requires the reviewed history-table rebuild
and private fine-resolution archive; lowering `user_version` alone is unsafe.
Older native clients cannot display Platform fields, and older servers cannot
continue Platform-owned opaque sessions. Reconcile the chosen rollback target
with client-held state before resuming workers; never strip proofs to fake
portability. Keep the same signing/encryption keys throughout.

## Schema 21 upgrade and rollback

Stop/drain workers and verify a private snapshot before schema 20 → 21. The
additive `routing_resources` table starts empty, preserving the previous policy
until the operator sets a resource selection. No credentials, route identities,
quota observations or usage rows are changed. Deploy matching server and native
gateway binaries; use the reviewed schema migration rather than the ordinary
same-schema Docker update.

To roll back, drain workers, take a current verified snapshot, remove the new
`routing_resources` setting/flag from the startup configuration, and set
`PRAGMA user_version=20` during maintenance before restoring matching schema-20
binaries. Retain the new table and its row so re-upgrading restores the selection.
The old binary uses the old fallback setting and **cannot enforce a single-resource
policy**. Review the resulting admission policy before reopening traffic.

## Schema 20 upgrade and rollback

Stop/drain workers and create a verified private snapshot before schema 19 → 20.
The additive migration creates manual reconciliation and numeric per-request
token-estimate tables plus an index, without changing OAuth, signing keys,
Platform credentials, uncertain-operation fences or existing usage rows. No
manual amount is inferred from the plan, and no upstream mutation is triggered.
The image and native gateway must both declare/support schema 20.

For rollback, preserve a current verified snapshot, stop workers and restore
matching schema-19 binaries/gateway with `PRAGMA user_version=19`. **Retain both
new tables and the usage index**; there are no new columns on existing tables.
Re-upgrade is idempotent and preserves manual epochs and frozen cost rows.
Platform requests processed by the older binary have no frozen token estimate,
so they remain unpriced after re-upgrade until the operator enters a new manual
balance. Never replace newer usage with an old backup just to roll back code.
This requires an explicit reviewed migration, not the ordinary same-schema
Docker update path.

## Schema 18 upgrade and rollback

Stop workers and take a verified private snapshot before upgrading schema 17 → 18.
The additive migration creates only Platform policy, settings, observation,
working-key and uncertain-operation tables. Existing OAuth/token keys, account
policy, inference accounting and Codex history are unchanged. No upstream key is
created by the migration; management requires explicit project configuration.

For reviewed rollback, stop all workers, preserve a current verified snapshot,
set `PRAGMA user_version=17` in a maintenance transaction and restore matching
schema-17 binaries/gateway. **Retain every schema-18 table**, particularly the
uncertain-operation fence and encrypted working keys. Do not restore an old
database over newer accounting or rotate keys. Schema-17 binaries ignore the
new tables; re-upgrade reuses them idempotently. Ordinary packaging updates must
still reject schema mismatches instead of performing this migration implicitly.

## Schema 19 upgrade and rollback

Schema 18 → 19 adds `platform_routes` and `platform_route_health`. It does not
rotate credentials, change the default Codex fallback policy or submit inference.
Stop workers and take a verified private snapshot before upgrading. Route IDs are
monotonic and must never be reset/reused: issued ownership proofs depend on them.

Schema-18 binaries do not support Platform context proofs or API inference.
Before rollback, stop all workers and preserve the current verified database.
Retain both schema-19 tables and `sqlite_sequence`; only then set
`PRAGMA user_version=18` in a maintenance transaction and restore matching binaries
and gateway. Platform-owned sessions cannot continue on that older binary; clients
must supply self-contained text/tool context for native routing. Do not discard
checkpoints or silently replay them on Codex. Re-upgrade reuses retained route
identities and accounting. One-click packaging must still reject a schema change.
