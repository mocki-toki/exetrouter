---
name: exr
description: "Use ExetRouter's exr CLI for standalone/remote access, models, usage and tokens; generate or edit ChatGPT images with the bundled helper; connect Codex/OpenCode and OpenAI SDK clients."
---

# ExetRouter client

On macOS, install the ready-made client with `brew tap mocki-toki/exetrouter https://github.com/mocki-toki/exetrouter` followed by `brew install mocki-toki/exetrouter/exr`; no Rust is required. Use the installed `exr` binary. If working in its Rust repository, build with `cargo build --locked --bins` and use `target/debug/exr`. Run `exr --help` and subcommand help to inspect the installed version's contract.

## Mode and first run

`exr` opens a first-run wizard when unconfigured, then an English dashboard. Settings edits the connection/mode. Standalone embeds its own loopback API and manages local OAuth accounts; no SSH or separate exrd process is needed. Remote mode connects to an operator-managed server. Reuse the saved mode and state; do not switch an existing profile without the user's intent.

```sh
exr configure --standalone
exr account list --json
exr account add
exr account reauth ID
exr account disable ID --yes
exr serve
```

Standalone data defaults to `$XDG_DATA_HOME/exr` (`~/.local/share/exr`), private mode 0700. `--state-dir PATH` and `--listen 127.0.0.1:PORT` override it. The API runs while the TUI or foreground `exr serve` remains open. Issue a local bearer in Tokens, then use the local `/v1` endpoint in your client. No user/SSH administration is exposed in standalone; the internal owner scope keeps token/usage/affinity semantics consistent.

OAuth add/reauth requires the owner's interactive terminal or TUI Settings browser flow. Never capture real tokens. Enter opens read-only Overview account details, Tokens actions and Settings actions. Account availability belongs to the operator or standalone Settings; deactivation retains OAuth state, and reauthorization enables a disabled local account again. Routing and expiring-reset redemption are automatic, with reasons/outcomes in Overview and Limits; there are no personal activation/label, manual priority, switching-rule or redemption controls. Short-window subscriptions are used before weekly-only reserves, except urgent reset-expiry/weekly-refresh deadlines within 72 hours. Soft transfers require full recoverable context. Settings supports account selection, add and reauthorize. Operator/server tasks belong to `$exrd`; schema 12 needs the reviewed migration in `docs/automatic-routing-migration.md`.

## Connection and inspection

Read existing connection settings; reuse them. Configure only when needed:

```sh
exr configure --identity /absolute/path/to/private-key
exr doctor --json
exr tokens --json
exr models --json
exr usage --period week --by model --json
exr usage --period week --by token --json
```

Configuration is `$XDG_CONFIG_HOME/exr/config.json`, default `~/.config/exr/config.json`. `--config PATH` overrides `EXR_CONFIG`; command-line host/port/ssh-user/identity override saved settings. Defaults: `localhost:2222`, user `routercli`. Key registration is an operator action. The interactive connection wizard checks SSH before saving and lets OpenSSH ask for first-time host-key confirmation in the same terminal; a human must verify the fingerprint with the operator before accepting. Normal requests still require a verified known_hosts entry. Load a protected key into ssh-agent for normal dashboard use. Never disable host-key verification.

Use `--json` for parsing. Default outputs are readable text without character-art charts; `exr` without a command opens the English TUI and requires a terminal. `tokens` aliases `token`, defaulting to list; `quota` aliases `limits`.

Doctor is a read-only snapshot, not a TLS/upstream/inference probe. Nonzero exit may still include a valid diagnostic JSON report. `exr limits --json` fetches live usage/reset-credit availability and labels accounts by email; run it only within authorized inference scope because of the weekly-activation exception below. A fresh 100%-remaining weekly window with a reset exactly seven days ahead at minute precision may trigger one minimal gpt-5.6-sol request on that enabled account. Attempts are durably limited to once per seven days; Doctor, catalog and update checks remain read-only. Window labels: 480-minute windows are 8-hour, 10080-minute windows weekly. Do not infer duration from primary/secondary, sum percentages across subscriptions or treat stale/reset-elapsed values as current availability. Unknown usage is not zero usage; cached/reasoning counters are subsets of input/output.

## Tokens

```sh
exr token show tok_ID --json
exr token create --name DEVICE --expires-days 90
exr token rotate tok_ID
exr token revoke tok_ID --yes
```

Creation/rotation require terminal stdin/stdout and prohibit JSON. Use the user's interactive terminal for issuance; never capture/paste a real bearer secret into agent output, logs, repository files or command arguments. The client copies the secret directly to the local desktop clipboard without printing it; the user pastes it into their secret store. Headless issuance is unavailable. A TUI clipboard failure offers c to retry copying without repeating issuance. Rotation immediately invalidates the old token. Revoke/rotate only the token within the user's requested scope.

SSH has a 45-second deadline. Never automatically repeat a token mutation after a timeout/error: inspect metadata first; an issued secret cannot be retrieved later. SSH registration does not give users access to other users' tokens or upstream credentials. Usage by user/model is shared aggregate metadata. Token grouping shows only your own token IDs, including revoked-token history; in the dashboard Usage view, b selects grouping and Up/Down selects a token.

Read [references/connections.md](references/connections.md) when connecting Codex, OpenCode or an SDK, for model metadata and endpoint configuration. Configuration/token installation into another client is a separate user-directed change; the CLI does not do it automatically.

## Generate or edit an image

Prefer the ExetRouter MCP server when connected: discover `generate_image` and `edit_image` through its tool list and invoke them directly. It uses the existing router bearer at `/mcp`, returns image content, and needs no built-in Codex image authorization. Read `references/connections.md` for setup; connecting another client's MCP configuration is a separate user-directed change. New server tools appear on catalog refresh/reconnect, subject to client permissions.

Use bundled `scripts/image.py` with Python 3, resolving its path relative to this skill (not the project). It requires a running image-capable ExetRouter API, an existing bearer in `EXETROUTER_TOKEN`, and `EXETROUTER_BASE_URL` including `/v1` (default loopback port 8787). Never print or put the bearer in arguments. Send the requested image prompt on stdin and pass `--output NEW_FILE`; add `--reference LOCAL_IMAGE` up to five times for editing. Optional `--quality`, `--size` and `--background` pass to `gpt-image-2`. Read the returned JSON path, then inspect/show the saved image with the agent's image viewer. Do not overwrite user files or blindly retry an interrupted request.

The helper uses ChatGPT OAuth through the router, not another provider or host LLM. It prints only the saved path/MIME type and never writes a response JSON/base64 file. Codex 0.165.0's built-in `image_gen` still has a client-side OAuth gate for custom bearer providers; do not fake auth state to enable it. Server schema 13 requires the reviewed migration in `docs/native-tools.md`; a newly published helper does not upgrade an existing server.

## Reset credits

`exr limits --json` inspects availability; it does not itself redeem credits. The running API consumes an expiring credit at <72 hours to expiry and ≤1% weekly remaining, or one final-day salvage for the already-expiring cohort with consumed ordinary quota. Further credits wait for new weekly depletion after recovery. Strategic resets do not affect priority or get spent automatically; other subscriptions need not be empty. Ordinary routing uses quota through exhaustion.

In standalone mode only, `exr account reset ACCOUNT` prepares one credit by local ID or exact email when live weekly quota has at most 5% remaining. The owner must review and type `RESET ID` in their own terminal. There is no `--yes` or `--json`, no TUI action and no remote request. Do not automate confirmation or retry an uncertain result.

A randomized delay and fresh preflight precede a durable submission with an explicit credit and an account/weekly-cycle idempotency key shared across hosts. Every potentially successful reset fences further credits until verified recovery and a changed weekly cycle. Unknown outcomes are never replayed; confirmed no-ops require new consumption before reconsideration. Cohort/inventory guards survive restart. Missing/expired/stale data cannot authorize redemption. Never spend a real credit deliberately for testing; starting the runtime enables its documented automatic behavior.

## Software updates

Use `exr update --check --json` for a read-only release check. Install only within the user's requested update scope. Built-in update preserves config/state/credentials; reopen the dashboard afterward. Homebrew uses `brew upgrade mocki-toki/exetrouter/exr`; source installs rebuild a locked published tag. Settings offers Check updates and Update exr. Server updates belong to the operator and the separate `exrd` skill.
