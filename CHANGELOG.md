# Changelog

## 0.8.3

- Refactor the experimental OpenCode Desktop plugin (0.2.0) into a compact native-component panel with Overview, Usage, Tokens, Models and Settings. Add TUI-equivalent native actions, connection editing, OAuth and clipboard-safe token management through a private bounded stdio bridge; never return token secrets to the GUI.
- Keep snapshots, account dialogs, drafts, filters and scroll stable during background refresh. Use one shared refresh action, highlight available updates first in Settings, preserve proportional typography, and add a contained activity chart with offset hover/keyboard values and concise timestamps.
- Show the actual current chat's account quotas in the native composer indicator instead of the global Best recommendation. Add opt-in HTTP/per-turn WebSocket account observations using user-scoped HMAC aliases, including socket reuse and account transfer, without new persistent state, public management endpoints or inference probes.
- Preserve routing, server-wide client activity and automatic-reset metadata when the native client decodes remote doctor reports. Keep missing counts unknown and avoid a false pool-wide cooldown when only some accounts are paused. No database schema change is required.

## 0.8.2

- Refresh catalog freshness without deleting/reinserting unchanged account model metadata. Preserve the 60-second refresh, active-generation fence, catalog changes, SQLite durability and accounting without a schema migration.
- Include the OpenCode V1 request-copy context plugin and its synthetic compaction-recovery tests, plus corrected Nix hashes for the pinned WebSocket dependencies.

## 0.8.1

- Make account `Clients` counts and activity lists server-wide. Keep the 15-minute idle window, running-request visibility and bounded lists; expose other users' clients only through stable keyed aliases, never private names or raw token/user identities. Keep `Best` and target shares identical for every viewer of the same server snapshot.
- Remove personal account activation and custom account labels from the dashboard, CLI and management protocol. Ignore retired personal availability records; preserve operator account controls, encrypted token names and dashboard preferences without a schema change.

## 0.8.0

- Add an experimental OpenCode Desktop provider and usage-dashboard plugin with a separate client build and test workflow; enabling it remains an explicit client-side action.
- Expose a bearer-authenticated `/mcp` Streamable HTTP endpoint with agent-discoverable `generate_image`/`edit_image` tools and native image results. Reuse native routing/quota/accounting, scope legacy cancellation to user/client sessions, and bound/cancel response streams without replay. Support modern and legacy MCP discovery; include Codex/OpenCode configuration, SDK fixtures and explicit ingress routing. New tools are discovered on catalog refresh/reconnect, not via a claimed live notification service.
- Add native Codex JSON image generation/editing and standalone search routes, with separate feature pauses, private user-scoped search affinity, bounded output and durable unknown-aware accounting. Include a shell-agent image helper in the exr skill; unmodified Codex's native image-tool auth gate remains an explicit limitation. Schema 13 requires the reviewed migration/rollback in `docs/native-tools.md`; no automatic deployment or live inference is implied.
- Remove router-defined inference request-size ceilings for HTTP, compressed/decoded zstd and client WebSockets, including bundled nginx ingress. Leave model/context validation to upstream while retaining authorization, concurrency, decoder deadlines and output/recovery bounds.
- Fragment outgoing upstream WebSocket requests without truncating context. Report upstream close 1009 as `upstream_request_too_large` without pausing the entire account, replaying inference or moving accounts.
- Negotiate upstream WebSocket `permessage-deflate` using the exact transport revisions pinned by Codex 0.164.0. Preserve plain fragmentation when declined and retain decoded upstream event bounds; do not replay a rejected inference.
- Select upstream HTTP SSE before submitting complete generation requests of at least 16 MiB, preserving the client's WebSocket, account affinity and full input. Recover HTTP continuations from the bounded latest window or request client-controlled full-context recovery before inference; never replay a WS size rejection or turn native warmup into generation.
- Compress large completed continuation windows in transient memory so HTTP-backed WebSockets can continue tool deltas without requiring full client reconstruction every turn. Keep the retained per-window/global budgets, bound decoded recovery separately, and preserve exact opaque/tool history, account checks and unknown usage.
- Distribute new independent dialogs within each eligible routing tier using remaining weekly quota and time until reset. Concentrate weekly quota in its final 24 hours; retain the separate 72-hour expiring-reset priority, short-window reserves and owned-dialog affinity.
- Show a green `Best` routing recommendation and per-account `Clients` counts. Account details list only the current user's recent API tokens, locally decrypted labels, running requests and last-request times; idle tokens disappear after 15 minutes without changing session ownership or usage history.

## 0.7.1

- Replace the reserve label with a red `Priority` marker for enabled accounts with urgent routing deadlines.
- Continue recoverable HTTP and WebSocket conversations on an eligible active account after personal, operator or standalone OAuth deactivation. Preserve encrypted history, tool results, concurrent forks and downstream session identity; discard old account-specific turn state.

## 0.7.0

- Replace manual account priorities and switching rules with automatic deadline-aware routing. Use short-window subscriptions before weekly-only reserves, return recoverable sessions to restored short windows, and prioritize expiring credits/weekly refreshes within 72 hours. Use ordinary quota through exhaustion; strategic reset inventory never affects ranking.
- Automatically spend only expiring reset credits: <72 hours and ≤1% weekly remaining, or one final-day salvage for the already-expiring cohort. Require new depletion before spending further cohort credits. Remove manual redemption controls.
- Add randomized live preflight, account/weekly-cycle cross-host idempotency, durable recovery and inventory fences, and safe reconsideration of confirmed no-ops. Verify recovery after successful as well as uncertain submissions. Document upstream consistency and independent-host history limits.
- Preserve quota-window absence ordering against late observations. Introduce schema 12 with a reviewed additive migration and rollback; routine Docker updates still refuse an unreviewed schema change.

## 0.6.0

- Remove the router's 24-hour opaque-context expiry, including for already-issued authenticated reasoning, compaction, encrypted tool arguments, turn state and saved conversation envelopes. Replace new per-element transfer registry writes with encrypted client-carried portability receipts, retaining legacy overrides without growing their registry. Preserve forks, restart/reopen, current authorization and account affinity without changing keys or schema. Older binaries cannot read the new transfer receipts; see the account-pool rollback contract.
- Preserve explicitly requested Responses Lite mode without requiring a visible tool prefix, including compaction, continuations and replacement WebSocket handshakes. Keep opaque-context ownership checks and arbitrary-header filtering unchanged.
- Authenticate and discard retired WebSocket turn state before checking full-context ownership after an authorized account transfer, avoiding false mixed-account errors when native clients replay their original turn token.
- Recover the Docker API and restricted SSH gateway after transient boot failures. Recreate the runtime directory before the systemd sandbox is prepared, retain bounded restart attempts and cover a missing boot-time runtime directory in the isolated unit fixture.

## 0.5.4

- Raise inference HTTP bodies, compressed and decoded zstd bodies, and incoming client WebSocket messages/frames from 16 MiB to 128 MiB. Match the bundled nginx templates and add large-request regression coverage. Keep output buffering and retained recovery history limits unchanged.
- Release stale subscription-reset cooldowns after a fresh live usage check explicitly allows requests. Fence concurrent rejections, retain other pause sources and report any retained cooldown honestly. Add the schema-10-to-11 per-account ordering fence and reviewed migration/rollback procedure; no automatic deployment or reset-credit consumption.
- Require the reviewed [schema 10 to 11 migration](docs/cooldown-recovery-migration.md) for existing servers; the Docker updater intentionally refuses a schema mismatch. Publishing this release does not migrate deployed servers.
- Bound and resume upstream compatibility evidence reads, avoid duplicate CI runs, and cache trusted main-build Rust dependencies.

## 0.5.3

- Order inference quota observations with the shared upstream operation clock, so live limits refreshes do not suppress later HTTP/WS updates. Preserve stale-observation and credential-generation fences without a schema change.
- Revoke the previous bearer immediately when rotating an API token.
- Rename encrypted token labels in the CLI/dashboard and show decrypted labels in user-scoped usage reports, including revoked tokens without merging identical names.
- Improve privacy-safe WebSocket interruption diagnostics and label the connection type in Settings.
- Add bounded upstream compatibility automation and isolated synthetic native-client fixtures; keep first-run dashboard tests independent of GitHub availability.

## 0.5.2

- Raise the bounded opaque-context snapshot limit from 128 to 16,384 unique values so long native histories can continue before compaction. Preserve ownership proofs, account affinity and request-size limits, and avoid quadratic reference deduplication.
- Add regression coverage for long reasoning/tool histories, foreign-user rejection and the exact snapshot boundary. No database schema change is required.

## 0.5.1

- Wait for modal dismissal in terminal integration tests before sending subsequent input, preventing Escape/character races on macOS runners.

## 0.5.0

- Stop writing a context binding for every output: return authenticated user/account envelopes for opaque context, turn state and saved conversation IDs, and restore original values before upstream dispatch. Require authenticated envelopes for all opaque continuations and preserve concurrent quota-transfer forks. Remove the former raw-context fallback and prune unpaired registry rows at startup. Full transfer storage fails before replacement inference instead of interrupting ordinary output.

- Encrypt native-client token names, personal account labels and dashboard preferences with an automatically created client-held key. Add bounded user-scoped ciphertext storage, automatic migration of existing token names, and the schema-9-to-10 migration/rollback plan.
- Support mouse navigation, token actions and model-ID copying, clickable controls and EN/RU command shortcuts without transliterating text input.
- Move Settings actions inline, put Configure connection in Connection, and replace Check updates with Update when a newer version is available.
- Publish matching schema-10 native gateway and container artifacts. Existing schema-9 servers require the reviewed migration and rollback procedure.

## 0.4.0

- Configure soft per-account switching thresholds for all, short and weekly quota windows through exr, exrd and the dashboard. Prefer alternatives at or below the remaining-percentage threshold, with fallback to healthy capacity and safe context transfer before submission.
- Replace the dashboard priority toggle with numeric priority -255…255 (default 1), inheritance and operator locks. Preserve stored priorities and credentials through the explicit schema-8-to-9 migration.
- Keep switching thresholds in the account settings form, edit priority there, and hide completed weekly-activation notices from ordinary account views.
- Service upstream and client WebSocket ping/pong, detect silent pre-response peers after 90 seconds, and retire recoverable upstream sockets between requests after 30 idle seconds or five minutes of age. Preserve live slow generations and same-account context recovery.
- Correlate client and upstream WebSocket lifecycle, request timing and interruptions using content-free IDs, fixed reasons and numeric liveness counters.
- Leave interrupted inference retry and transport fallback policy to clients instead of overriding native client defaults or wrapping WebSocket interruptions as HTTP 400 errors.
- Publish schema-9 native gateway and container artifacts together. Existing schema-8 installations require the reviewed account-routing migration rather than an automatic packaging update.

## 0.3.0

- Export dedicated OpenCode V1/V2 providers with pool models, native metadata and connection settings; reuse the standalone or saved remote API URL. Matching authenticated model routes expose provider fragments.
- Forward backend request options without a static capability denylist and translate Chat output caps. Preserve backend HTTP statuses with redacted HTTP/Chat errors, resource bounds and adapter checks; enforce user ownership and account pinning for saved conversation references.
- Activate apparently inactive weekly windows with one durably guarded minimal gpt-5.6-sol request; retain nullable counters in user/model/total usage and expose the attempt status. Requires the explicit schema-7-to-8 migration plan.
- Support Ctrl-C cancellation throughout connection setup without saving edits.
- Add a visible cursor and editing within connection fields, including Unicode and long paths.
- Check remote SSH settings before saving and allow verified first-time host-key confirmation in the same terminal.
- Add source-pinned Nix flake installation and development support.
- Simplify documentation, separate client configuration from launch and provide a dedicated Docker/native server installation guide.

## 0.2.2

- Select individual models, users or API tokens with recorded usage in the dashboard; user totals include all of their tokens, while token IDs remain scoped to the authenticated user.
- Default Usage charts to reported tokens and draw clearer lines with readable upper axis limits rounded upward.
- Keep the selected item in the chart legend, including full token IDs on small terminals; reserve the combined view for total grouping.
- Show keyboard help for the current dashboard tab.
- Simplify OpenCode V1/V2 configuration to the built-in OpenAI provider, removing the optional custom plugins.

## 0.2.1

- Simplify OpenCode V1/V2 setup to the built-in OpenAI provider with a router endpoint and token; remove the custom client plugins and retain standard OpenCode retries.

- Put Linux binary installation first in the README, followed by macOS Homebrew and source builds; give shared-server deployment its own section.

## 0.2.0

- Add a binary-only macOS Homebrew tap for exr, update its checksums from release artifacts, and preserve Homebrew ownership during client updates.

- Publish versioned Linux AMD64/ARM64 container images alongside native releases.
- Make Docker Compose the primary installation, administration and update interface; bootstrap directly with the container and document the separate native SSH gateway.
- Keep the host launcher optional and change its updater to pull images, pin digests, compare the live backup schema and validate binary versions before switching.
- Preserve legacy host-network overrides and health rollback without building Rust on the operator host.

## 0.1.3

- Fix Docker updates incorrectly refusing the existing seven-migration schema; future schema changes still require a planned operator upgrade.

## 0.1.2

- Exclude generated Python bytecode from Cargo packages.

## 0.1.1

- Add usage grouping by API token in the CLI and dashboard, scoped to the authenticated user and preserving revoked-token history and unknown usage.

## 0.1.0

- Initialize the consolidated ExetRouter source history.
- Include standalone and shared routing, OAuth account pools, Responses and limited text Chat Completions.
- Include account preferences, operator policy, contextual dashboard actions and bounded 16 MiB inference requests.
- Add bounded automatic quota account failover with full-context recovery, preserved tool results and user-scoped portability for concurrent forks.
