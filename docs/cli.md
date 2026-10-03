# Client CLI and TUI

`exr` is the macOS/Linux management client. All UI, help and documentation are in English. The current client implements readable command output and a terminal dashboard. JSON is opt-in through `--json`.

## Configure once

```sh
exr configure --identity ~/.ssh/exetrouter_ed25519
# Or set the entire connection explicitly:
exr configure --host api.example.com --port 2222 --ssh-user routercli \
  --identity ~/.ssh/exetrouter_ed25519
# With no connection flags, configure runs an interactive wizard:
exr configure
```

Defaults are host `localhost`, port `2222`, username `routercli`. The private key must already exist with owner-only permissions. The command saves connection metadata in `~/.config/exr/config.json` (`$XDG_CONFIG_HOME/exr/config.json` when set). It contains a key **path**, never the key material or an API token. Saving uses an atomic replacement and mode `0600`. Existing non-regular destinations are rejected.

Use `--config /path/to/config.json` or `EXR_CONFIG` for a separate connection. Precedence: command-line flags override the saved settings; `--config` overrides `EXR_CONFIG`, which overrides the default path. `~/` identity paths expand to the current home directory. A shell alias is unnecessary.

The operator must register your public Ed25519 key and provide a verified host-key fingerprint. Install the verified host key for `[api.example.com]:2222` in `known_hosts`. The client uses `StrictHostKeyChecking=yes`, `BatchMode=yes`, `IdentitiesOnly=yes`, disables forwarding and sends only `exrd-gateway`. Load a passphrase-protected key with `ssh-add` before use. Configure does not register keys or modify SSH settings.

## Commands

```sh
exr                         # Open dashboard
exr tui                     # Explicit dashboard command
exr doctor                  # Text diagnostics; nonzero exit for configuration issues
exr doctor --json           # Stable local/server diagnostic object
exr models                  # Human-readable table
exr models --json           # OpenAI-style catalog
exr models --json --format codex-json > /absolute/path/models.json
exr models --json --format opencode-jsonc > /absolute/path/models.jsonc
exr usage --period day
exr usage --period 24h
exr usage --period week --by user
exr usage --period week --by token
exr usage --period month --by model --json
exr account set 1 --priority 1 # Prefer an account for your user
exr account set 1 --enabled false # Reversible deactivation for your user
exr limits                  # Per-account upstream subscription windows
exr limits --json            # Array of live account limits and reset credits, labeled by email
exr tokens                  # List your tokens
exr token list --json        # Same list, for scripts
exr token show tok_...
exr token create --name macbook --expires-days 90
exr token rotate tok_...
exr token revoke tok_...     # Interactive confirmation
exr token revoke tok_... --yes
```

`tokens` is an alias for `token`; omitting its subcommand lists tokens. `quota` is an alias for `limits`. Expiration is 1–365 days, default 90. Creation and rotation require terminal stdin/stdout and reject `--json`. Rotation immediately revokes the old token; update the consuming service with the new secret. JSON/model exports contain no secrets. `--format` requires `--json`.

Token metadata and mutations are scoped to the authenticated user. Usage is shared aggregate metadata, grouped by user or model. Token grouping (`--by token`) shows only the authenticated user’s token IDs, including revoked tokens with recorded usage; users cannot list another user's tokens, inspect OAuth identities or read request contents. User creation, OAuth login and backups remain local `exrd admin` operations. Operator reports also default to readable text; add `--json` for scripts. OAuth login is interactive and rejects JSON. The SSH gateway wire protocol and raw authorized_keys export retain their dedicated machine formats.

SSH exchanges have a 45-second deadline and a 1 MiB reply cap. Failed child processes are killed and reaped. Failure or timeout does not prove a mutation was cancelled: inspect the token list before retrying creation/rotation/revocation. The client never automatically retries these operations.

## Dashboard

Start `exr` in an interactive terminal after configuration (minimum 72 columns × 20 rows). Views:

| View | Content and actions |
| --- | --- |
| Overview | Server connection, routing readiness, OAuth/catalog state, resource concurrency and per-account subscription limits. A stale catalog is labeled as cached and refreshed on the next model request. |
| Usage | Hourly/daily activity chart for Today, Last 24 hours, This week or This month, grouped by user, model or your own token IDs. Switch between requests and reported tokens; in token grouping, use Up/Down to select one token’s graph and totals. |
| Tokens | Select a token, inspect its metadata, create, rotate or revoke. Revoked tokens are hidden. |
| Models | Account-visible catalog in reverse display order and reported context/output limits. JSON/client exports preserve the catalog contract. |
| Settings | Mode/connection editor; standalone account add, reauthorization and disable. |

Keys: ←/→ change the five views; ↑/↓ select accounts in Overview, tokens, models or local accounts in Settings; PageUp/PageDown scroll; `r` refreshes the active view; `?` opens help; `q` exits. In Usage, `p` cycles Today → Last 24 hours → This week → This month, `b` cycles total/user/model, and `m` switches requests/reported tokens. Views and the bottom Controls area are borderless, with two-column content margins and a blank line above their contents. Refresh timestamps are hidden; action outcomes, errors and working indicators appear above the contextual key hints and belong to their own view. Long status messages wrap onto additional lines without hiding the key hints. The header shows ExetRouter and its version on the left, the connection on the right, then a blank line before navigation. The active tab is highlighted with one space on each side. Usage summaries and notes share the content margins; its graph retains a frame. In Overview, the selected account is highlighted and kept visible; Enter opens its actions menu, where reset-credit review checks live eligibility, `y` confirms and Esc/`n` cancels. Reset-credit errors appear in a dialog dismissed with Enter or Esc, without initiating another request. In Models, Enter copies the selected model ID to the desktop clipboard; the list scrolls with selection. Settings marks Remote Server in purple and Standalone in orange. In Tokens, Enter opens actions for the selected token: create, rotate or revoke. Rotation and revocation require a `y` confirmation. Esc cancels an input/confirmation dialog.

Creation and rotation copy the secret directly to the local system clipboard without displaying it. Paste it into your service or password manager. Clipboard support is checked before issuing a token: macOS uses `pbcopy`, Wayland uses `wl-copy`, and X11 uses `xclip` or `xsel`; headless terminals cannot issue tokens. If the helper fails after issuance, the TUI retains the secret only in memory and offers `c` to retry copying without another token request. Esc discards the secret; rotate the token later to obtain a new one. The CLI reports the token ID and copy failure so you can rotate it. There is no fallback that prints a secret or emits it as a terminal escape sequence. The client does not write secrets to files or logs. Ctrl-C exits; interrupting an outstanding mutation reports that its outcome may be uncertain. Terminal raw mode and the alternate screen are restored on normal exit and returned errors. A forcibly killed process cannot guarantee terminal cleanup; run `reset` if needed.

Views load on demand, with SSH work in the background. Switching away cancels an outstanding read; mutations are allowed to finish. Overview automatically polls live limit metadata every 30 seconds; other views load on demand. Refresh never generates inference. Freshness describes the age of the upstream observation, not the moment the screen was refreshed.

## Limits and diagnostics

`8-hour` is shown only for a reported 480-minute window; `Weekly` only for 10080 minutes. A 300-minute window is `5-hour`. Neither `primary` nor `secondary` implies a fixed duration. Overview shows subscription windows inline: shorter windows on the left and weekly on the right, with compact labels such as `5h` and `week`. Narrow terminals stack the windows; a single reported window uses the full width. Reset dates stay centered and omit the weekday when needed to fit a narrow column. Bars fill with colored spaces: remaining quota at 20% or below is red, above 20% through 50% is amber, and above 50% is green. Bright backgrounds use dark matching text; the empty gray portion uses gray text. Historical observations are labeled independently of the percentage color. Human views hide windows without a reported duration; JSON retains them unchanged. Unavailable counters and reset times remain unknown. Old percentages are marked `stale` or `reset_elapsed`, never replaced with fabricated zeros. In Overview, bars show the remaining percentage without an observation-date row. A `~… to limit` estimate appears only after at least one minute of fresh, increasing observations during this dashboard session. It uses recent percentage consumption, separately per account/window, and stays in memory only. Estimates disappear after five minutes without a change for short windows (including 5-hour and 8-hour), or thirty minutes for weekly/longer windows. Stale/failed refreshes, observation gaps over one minute, resets, insufficient data and a free reset before predicted exhaustion suppress estimates. No estimate does not prove zero consumption: upstream percentages may be rounded. Account labels show email addresses from issuer-provided encrypted credentials; missing claims are labeled `Email unavailable`. Numeric IDs identify local pool entries, not upstream account IDs. Percentages across accounts are not summed.

Doctor reads local metadata and decrypts credentials only to extract account email labels. It does not refresh tokens, make upstream calls or run inference. Its public TLS/WSS and upstream network status remain `not_checked`. The JSON report retains `schema_version=1` and the older single-account `quota` field; `quota_accounts` adds per-account summaries for pools. Older servers may only expose the single-account summary.

Human dates use English weekday/month names and the local time zone of the computer running `exr`; whole percentages omit `.0`. Terminal command colors respect `NO_COLOR`, and piped output has no ANSI styling.

The client sends its system IANA time zone for Usage calendar boundaries. The server adds hourly buckets for day/24h and daily buckets for week/month, including DST changes. `24h` is a rolling 86,400-second window ending at report time, with 24 hourly buckets; `day` starts at local midnight. Legacy requests without a time zone retain `Europe/Moscow` boundaries. JSON keeps Unix timestamps and counters; the `timeline` field is additive. Cached tokens are included in input; reasoning tokens are included in output. Missing counters are `null` in JSON and `—` in human output. Token plots sum reported input/output only and flag incomplete usage; missing counters are not presented as zero consumption. See [usage accounting](implementation.md#usage-accounting).

## Reset credits

Overview refreshes live limits automatically every 30 seconds while visible, when no command or confirmation is pending. Returning to Overview also refreshes. `exr limits` and the Overview view refresh live usage and reset-credit availability through authenticated Codex usage endpoints. A failed refresh retains historical quota observations and reports that live availability is unavailable. Doctor remains a local snapshot; its credit metadata, when present, may be cached. No inference is used to measure quota.

Run `exr limits reset <email>` in an interactive terminal, or open the selected account with Enter in Overview and choose Review reset credit. Duplicate emails can be distinguished by local numeric account ID. The server requires a current ordinary subscription window with **≤5% remaining**, a known future reset time and an available reset credit. Unknown/elapsed windows are ineligible. Unsupported or expired detail rows cannot be consumed. The earliest-expiring supported credit is selected; if details are unavailable but usage reports credits, the upstream can select its next credit.

Review the account, remaining percentage, credit expiry and free reset time, then type `yes` in the CLI or `y` in the TUI. When an eligible window resets in less than three days, the confirmation recommends saving the credit and waiting for the free reset. There is no `--yes` bypass and reset consumption rejects `--json`.

Confirmation belongs to the SSH user, expires after two minutes and can be submitted only once. The server rechecks live limits, credit availability, account generation and SSH authorization before sending the idempotent consume request. It does not automatically retry a mutation. On timeout or an ambiguous response, inspect live limits before any further action. Service restart invalidates pending confirmations. Credit consumption is verified against mock endpoints; production credits are never spent by automated tests.

## First run and standalone

Running `exr` with no connection opens a wizard: choose Standalone or Remote, edit fields (Ctrl-U clears one), review and confirm. Esc cancels without saving. Settings (`←/→`) changes connection/mode. Standalone uses an embedded loopback API, private local credentials and no SSH, remote daemon or user administration.

Settings: Enter opens actions to configure the connection, check/install updates and, in standalone, add or reauthorize accounts. `↑/↓` selects an account. Use Tokens to create a local API key. Account operations are also available as `exr account list/add/reauth/disable`; `exr serve` keeps the embedded API running without TUI. The API stops when its owning dashboard or serve process exits. If exr serve already owns the profile, the dashboard attaches without starting a duplicate API. Ordinary command reports contain text/numbers; charts and character-art widgets appear only in TUI.

## Software updates

The dashboard checks for a published GitHub release once per session at startup. The client version appears beside ExetRouter in the top header; **Settings ↑** is highlighted when an update is available. `v` checks again; Enter opens the Settings actions menu; choose Update exr to open confirmation. After installation, close and reopen exr. Automatic checks can be disabled with `EXR_NO_UPDATE_CHECK=1`. They do not contact the router or send account/usage data. CLI equivalents are `exr update --check [--json]` and `exr update`. A remote client updates itself; server operators use `exrd update` on the host. See [update methods and compatibility](installation.md#updates).
