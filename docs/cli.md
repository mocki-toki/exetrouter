# Client CLI and TUI

The dashboard supports mouse clicks on tabs, account/token/model rows, action menus and bottom controls. Clicking an account opens its details; clicking a token opens its actions; clicking a model selects it and copies its ID, just like Enter. Settings actions are inline: Check updates appears first and becomes Update when an update is available; Configure connection is in Connection. Use Up/Down and Enter or click an action. Bottom buttons and action-menu entries display their shortcuts. Buttons wrap on narrow screens; Controls contains navigation hints without repeating button shortcuts. The wheel uses the same selection/scroll behavior as Up/Down. Dialog buttons retain explicit confirmation for token rotation/revocation and updates, and block clicks on the background. Token-label fields support cursor placement. All keyboard controls remain available, with `1`–`5` as direct tab shortcuts. Help accepts `?` or `/` (also the Russian-layout `.`/`,` on the same key). EN/RU command shortcuts work in either layout, including action menus and confirmation dialogs; text fields are not transliterated. Other physical-key layouts cannot be inferred from ordinary terminal character events. Terminal text selection usually requires Shift while mouse capture is active, depending on the terminal. Connection setup and browser sign-in remain keyboard-driven.

`exr` runs a standalone API or connects to a shared server on macOS/Linux. Run it without a command to open the dashboard. CLI reports use readable text; add `--json` for scripts.

Token names and dashboard preferences are encrypted automatically by the native client. Existing token names are encrypted the next time the token list is loaded. Accounts display issuer-provided email labels. See [encrypted personal data](private-metadata.md) for key backup and recovery.

## First run and standalone

Running `exr` with no connection opens a wizard: choose Standalone or Remote, edit fields (Ctrl-U clears one), review and confirm. Esc cancels without saving. Settings (`←/→`) changes connection/mode. Standalone uses an embedded loopback API, private local credentials and no SSH, remote daemon or user administration.

Settings: Enter opens actions to configure the connection, check/install updates and, in standalone, add or reauthorize accounts. `↑/↓` selects an account. Use Tokens to create a local API key. Account operations are also available as `exr account list/add/reauth/disable`; `exr serve` keeps the embedded API running without TUI. The API stops when its owning dashboard or serve process exits. If exr serve already owns the profile, the dashboard attaches without starting a duplicate API. Ordinary command reports contain text/numbers; charts and character-art widgets appear only in TUI.

## Configure once

```sh
exr configure --identity ~/.ssh/exetrouter_ed25519
# Or set the entire connection explicitly:
exr configure --host api.example.com --port 2222 --ssh-user routercli \
  --identity ~/.ssh/exetrouter_ed25519 --api-url https://api.example.com/v1
# With no connection flags, configure runs an interactive wizard:
exr configure
```

Defaults are host `localhost`, port `2222`, username `routercli`. The private key must already exist with owner-only permissions. The optional HTTP API URL can also be edited in Settings or the setup wizard. The command saves connection metadata in `~/.config/exr/config.json` (`$XDG_CONFIG_HOME/exr/config.json` when set). It contains a key **path**, never the key material or an API token. Saving uses an atomic replacement and mode `0600`. Existing non-regular destinations are rejected.

Use `--config /path/to/config.json` or `EXR_CONFIG` for a separate connection. Precedence: command-line flags override the saved settings; `--config` overrides `EXR_CONFIG`, which overrides the default path. `~/` identity paths expand to the current home directory.

The operator must register your public Ed25519 key and provide a verified host-key fingerprint. The interactive connection wizard checks SSH before saving and lets OpenSSH ask you to confirm a new host fingerprint in the same terminal. Compare it with the operator’s verified fingerprint before accepting. Scripted configuration still requires a verified `[api.example.com]:2222` entry in `known_hosts`. Normal requests use `StrictHostKeyChecking=yes`, `BatchMode=yes` and `IdentitiesOnly=yes`, disable forwarding and send only `exrd-gateway`. Load a passphrase-protected key with `ssh-add` before use. `exr configure` does not register keys or modify SSH settings.

## Commands

```sh
exr                         # Open dashboard
exr tui                     # Explicit dashboard command
exr doctor                  # Text diagnostics; nonzero exit for configuration issues
exr doctor --json           # Stable local/server diagnostic object
exr models                  # Human-readable table
exr models --json           # OpenAI-style catalog
exr models --json --format codex-json > /absolute/path/models.json
exr models --json --format opencode-v1-json > /absolute/path/exetrouter-v1.json
exr models --json --format opencode-v2-json > /absolute/path/exetrouter-v2.json
exr usage --period day
exr usage --period 24h
exr usage --period week --by user
exr usage --period week --by token
exr usage --period month --by model --json
exr usage --period 90d --by model --json
exr usage --period 7d --before 1731283200 --json # Earlier rolling interval
exr quota-history 1 --period 30d --json         # Recorded metadata only
exr account disable 1 --yes  # Standalone only; reauthorize to enable again
exr limits                  # Per-account upstream subscription windows
exr limits --json            # Array of live account limits and reset credits, labeled by email
exr tokens                  # List your tokens
exr token list --json        # Same list, for scripts
exr token show tok_...
exr token fast-mode tok_... off    # on/off/inherit server default
exr token disable tok_...        # Reversible; secret remains unchanged
exr token enable tok_...
exr tokens rename tok_... "Work laptop"
exr token create --name macbook --expires-days 90
exr token rotate tok_...
exr token revoke tok_...     # Interactive confirmation
exr token revoke tok_... --yes
```

`tokens` aliases `token`; both list tokens when no subcommand is given. `quota` aliases `limits`. Token expiration is 1–365 days, default 90. Creation and rotation require terminal stdin/stdout and reject `--json`; rotation revokes the old token immediately.

The OpenCode Desktop Usage tab also plots a [standard API USD equivalent](usage-pricing.md) beside tokens and requests. Each model is priced before aggregation, so Astra and Sol are not treated as equally expensive per token. Unknown and unpriced requests remain visible as incomplete estimates; this is not a ChatGPT subscription bill.

Desktop Usage and Limit history offer **24 hours, 7 days, 30 days and 90 days**
with previous/next interval arrows. The CLI retains calendar periods for existing
scripts and also accepts rolling `7d`, `30d`, `90d`; `--before` selects an exclusive
Unix end for rolling windows only. Their buckets are fixed hours/days, not local
calendar days. Token rows show metadata inline, with status and Fast badges next
to each other. The action menu uses **Fast mode → Default / Allow / Block**;
Default inherits the server setting. Disable/Enable is reversible and preserves
the token secret; Revoke is permanent.

Model exports require `--json`. OpenCode exports use the standalone address or saved remote `api_url`; `--base-url` overrides one export. URLs must use HTTP(S), end in `/v1`, and have no credentials, query or fragment. `--model exetrouter/gpt-5.6-sol` selects an available model without changing the exported list. See [client setup](compatibility.md#opencode-setup) and [export examples](model-export-examples.md).

Token metadata and mutations are scoped to the authenticated user. Usage is shared aggregate metadata, grouped by user or model. Token grouping (`--by token`) shows only the authenticated user’s tokens by their current labels, including revoked tokens with recorded usage. Grouping stays keyed by token ID, so identical labels never merge statistics. JSON retains each ID in `name` and adds the client-decrypted `display_name`. Unavailable labels (including reports from older servers without label metadata) display `Label unavailable`, never a token fragment. Users cannot list another user's tokens, obtain upstream credentials/account identifiers or read request contents. User creation, OAuth login and backups remain local `exrd admin` operations. Operator reports also default to readable text; add `--json` for scripts. OAuth login is interactive and rejects JSON. The SSH gateway wire protocol and raw authorized_keys export retain their dedicated machine formats.

SSH exchanges have a 45-second deadline and a 1 MiB reply cap. Failed child processes are killed and reaped. Failure or timeout does not prove a mutation was cancelled: inspect the token list before retrying creation/rotation/revocation. The client never automatically retries these operations.

## Dashboard

Use an interactive terminal at least 72 columns × 20 rows. Press `?` for help on the current tab.

| View | What you can do |
| --- | --- |
| Overview | Inspect routing, limits and recently active clients. Enter opens read-only account details, including token labels and last-request times. |
| Usage | View Today, Last 24 hours, This week or This month, grouped by total, user, model or your API tokens. |
| Tokens | Create, rename, rotate or revoke tokens. Revoked tokens are hidden. Enter opens actions. |
| Models | Browse account-visible models and reported limits. Enter copies the selected model ID. |
| Settings | Change mode/connection, check/install client updates, and add or reauthorize standalone accounts. Enter opens actions. |

←/→ changes tabs; ↑/↓ selects items; PageUp/PageDown scrolls; `r` refreshes; `q` or Ctrl-C exits. In Usage, `p` changes the period, `b` changes grouping and `m` switches between reported tokens and requests. Select a group with ↑/↓ to see its chart and totals. User groups combine all that user's tokens; token groups show only your own token labels.

Tokens: Enter → Rename token (or `l`) edits the selected label. Enter saves, Esc cancels, and Ctrl-U clears the field. Labels must be 1–80 bytes without control characters. Renaming encrypts the new label on the client and changes neither the bearer secret, token ID, expiry nor usage counters; past usage displays the new label on reload. CLI renaming also supports `--json` and requires no clipboard or interactive terminal.

Overview shows a red `Priority` label for enabled accounts with an urgent reset-expiry/weekly-refresh deadline, along with the latest reset outcome. Ordinary reserve accounts have no routing label. Account availability belongs to the server operator or standalone Settings; personal activation and custom account labels are not supported. After deactivation, recoverable continuations automatically move to an eligible active account. There are no manual priority, switching-rule or reset-redemption actions. Soft transfers preserve a healthy original account when complete context or an eligible replacement is unavailable. See [automatic selection](account-pool.md#ranking-and-switching).

Green `Best` identifies the server-wide preferred general-pool candidate for a new dialog, not the account every request must use. Details explain the reason and approximate target share; model support, cache hints and existing affinity can change selection. `Clients: N` counts all users' unique API tokens with inference activity on that account in the past 15 minutes, plus tokens with running requests. It does not count connected applications or idle sockets. Details show your own locally decrypted token labels/IDs and stable anonymous aliases for other users' clients, with running-request counts and local last-request times. Other users' names, raw token IDs and user identities remain private. Older servers retain their explicitly user-scoped activity display. Use ↑/↓ or PageUp/PageDown to scroll, `r` to refresh and Esc to close.

Connection fields support Left/Right, Home/End (Ctrl-A/Ctrl-E), Backspace/Delete and Ctrl-U to clear. Up/Down selects a field. Esc cancels edits; Ctrl-C exits without saving. Remote settings are saved after a successful SSH check.

Token rotation and revocation need `y` confirmation. Esc cancels dialogs. Action results and errors appear above the Controls hints.

### Clipboard and uncertain mutations

Token creation/rotation copies the secret directly to the local desktop clipboard. Paste it into your service or password manager. Support is checked before issuance: `pbcopy` on macOS, `wl-copy` on Wayland, or `xclip`/`xsel` on X11. Headless terminals cannot issue tokens. Secrets are never printed, written to files/logs or sent through terminal escape sequences.

If copying fails after issuance, the TUI keeps the secret in memory and offers `c` to copy again without creating another token. Esc discards it; rotate later to obtain a new secret. The CLI reports the token ID and copy failure.

An interrupted mutation may have completed. Inspect token metadata before retrying; the client never retries automatically. Normal exit restores the terminal. After a forced kill, run `reset` if needed.

Views load on demand with SSH work in the background. Switching tabs cancels reads and lets mutations finish. Overview refreshes live limits and client activity every 30 seconds when no command or confirmation is pending, including while account details are open. Freshness refers to the upstream observation, not the screen refresh.

## Limits and diagnostics

Window labels use reported durations: 480 minutes is **8-hour**, 10080 is **Weekly**, and 300 is **5-hour**. Primary/secondary labels do not imply a duration. Human views hide windows without a reported duration; JSON preserves them. Missing counters/reset times stay unknown; historical percentages are marked `stale` or `reset_elapsed`. Percentages across accounts are never summed.

Overview shows remaining quota with red bars at ≤20%, amber through 50%, and green above 50%. Narrow terminals stack windows. Account labels use issuer-provided email metadata; missing claims show `Email unavailable`. Numeric IDs refer to local pool entries.

A `~… to limit` estimate needs at least one minute of fresh, increasing observations in the current dashboard session. It is kept only in memory and disappears after five minutes without change for short windows, or thirty minutes for weekly/longer windows. Failed/stale reads, gaps over one minute, resets, insufficient data or an earlier free reset suppress it. Rounded upstream percentages can hide consumption, so a missing estimate does not mean no usage.

Doctor reads local metadata and decrypts credentials only to extract account email labels. It does not refresh tokens, make upstream calls or run inference. Its public TLS/WSS and upstream network status remain `not_checked`. The JSON report retains `schema_version=1` and the older single-account `quota` field; `quota_accounts` adds per-account summaries for pools. Older servers may only expose the single-account summary.

Human dates use English weekday/month names and the local time zone of the computer running `exr`; whole percentages omit `.0`. Terminal command colors respect `NO_COLOR`, and piped output has no ANSI styling.

The client sends its system IANA time zone for Usage calendar boundaries. The server adds hourly buckets for day/24h and daily buckets for week/month, including DST changes. `24h` is a rolling 86,400-second window ending at report time, with 24 hourly buckets; `day` starts at local midnight. Legacy requests without a time zone retain `Europe/Moscow` boundaries. JSON keeps Unix timestamps and counters; the `timeline` field is additive. Cached tokens are included in input; reasoning tokens are included in output. Missing counters are `null` in JSON and `—` in human output. Token plots sum reported input/output only and flag incomplete usage; missing counters are not presented as zero consumption. See [usage accounting](implementation.md#usage-accounting).

## Weekly activation

A fresh weekly window (10080 minutes) with **100% remaining** and a reset exactly seven days after the observation, compared at minute precision, is treated as apparently inactive. Live limits submit one minimal `gpt-5.6-sol` Responses request on that exact account to start the window, then reread usage. The account must be enabled by operator policy, operationally available and support this model. A recent ordinary request suppresses activation. Requests use low reasoning, no tools and a fixed short prompt; generated content is discarded. Catalog, Doctor, update checks and the automatic metadata worker remain free of inference.

The attempt is recorded before submission and limited to once per account in seven days across users, processes and restarts. Only one activation runs at a time, with a 20-second inference deadline and 64 KiB response limit. Overview and readable limits show the latest attempt status for seven days. Nullable numeric usage is retained and included in total, model and user reports; these requests have no API token and are excluded from token grouping. This pattern is a heuristic, not an upstream guarantee that a zero-percent window is inactive. Schema 8 requires the [migration and rollback plan](quota-activation-migration.md).

## Reset credits

`exr limits` and Overview read live usage and reset-credit availability. A failed read keeps historical observations and reports the refresh error. Doctor remains a local snapshot, including any cached credit metadata.

The running server/standalone API manages resets automatically, even with the dashboard closed. It spends only supported, available credits with a known expiry. At **less than 72 hours to expiry**, a current weekly window with **≤1% remaining** qualifies, regardless of other subscriptions' capacity. During the final **24 hours**, consumed current ordinary quota qualifies for one early salvage, even above 1% weekly remaining. A fully unused account is not reset. Credits already in the final-day cohort must then wait for new weekly depletion to ≤1% until that cohort expires; a little new usage cannot consume the entire stock. Short-window exhaustion alone does not qualify a 24–72-hour credit. Longer-lived strategic credits are never automatically consumed. The 1% condition is for redemption only: normal routing consumes available quota through exhaustion.

The earliest-expiring supported credit is selected with a deterministic ID tie-break. Before POST, wait a randomized delay of up to one minute, then fetch fresh usage and credit inventory again. Cancel if the selected credit or weekly cycle changed, eligibility changed, authorization/generation changed or operational backoff applies. Independent installations derive the same upstream UUID idempotency key from the upstream account and its weekly reset deadline, and always send an explicit `credit_id`. Different credits observed against the same unrecovered weekly cycle therefore share one redemption key. Random timing alone is not duplicate-consumption protection; correctness depends on upstream honoring redemption idempotency.

Persist digest-only cycle/credit identities and a numeric quota snapshot before submission. Every potentially successful outcome, including `reset` and `already_redeemed`, blocks another credit until fresh explicitly allowed limits show >1% in all reported windows, a strict usage decrease in every previously consumed window and a changed weekly reset deadline. An unchanged deadline does not authorize another automatic redemption. Uncertain requests are never replayed, including after restart; verified recovery is recorded separately from claimed success. A confirmed `nothing_to_reset`, `no_credit` or rejection does not permanently spend the credit: fresh additional consumption may rearm it after at least 30 seconds, reusing the same cycle key.

Inventory disappearance on a monitoring host independently fences the next credit and records the final-day cohort, including when another host performed the reset. These observations and recovery/cohort fences survive restart. This is not a shared distributed ledger: an independently initialized host cannot reconstruct another host's previous salvage history from an available-only inventory. Use remote clients of one authoritative runtime when shared cohort history is required. Reads cannot guarantee an atomic snapshot across upstream endpoints; missing or contradictory recovery evidence defers further redemption.

Read limits after the response; never infer new percentages from a successful POST. Matching subscription cooldowns recover only through the existing fenced live-usage path. Manual `limits reset`, reset confirmations and routing flags are removed; old management redemption requests fail explicitly. Tests use synthetic endpoints only. Doctor and Limits do not themselves redeem credits, but the runtime worker operates independently of those reads. See [schema 12 deployment/rollback](automatic-routing-migration.md).

## Software updates

The dashboard checks for a published GitHub release once per session at startup. The client version appears beside ExetRouter in the top header; **Settings ↑** is highlighted when an update is available. `v` checks again; Enter opens the Settings actions menu; choose Update exr to open confirmation. After installation, close and reopen exr. Automatic checks can be disabled with `EXR_NO_UPDATE_CHECK=1`. They do not contact the router or send account/usage data. CLI equivalents are `exr update --check [--json]` and `exr update`. A remote client updates itself; server operators use `exrd update` on the host. See [update methods and compatibility](installation.md#updates).
