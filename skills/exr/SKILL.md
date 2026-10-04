---
name: exr
description: "Use ExetRouter's exr CLI to use standalone or remote mode, configure access, inspect models, usage and upstream limits, and manage router tokens; includes Codex/OpenCode and OpenAI SDK connection guidance."
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
exr account enable ID
exr account set ID --priority 1
exr serve
```

Standalone data defaults to `$XDG_DATA_HOME/exr` (`~/.local/share/exr`), private mode 0700. `--state-dir PATH` and `--listen 127.0.0.1:PORT` override it. The API runs while the TUI or foreground `exr serve` remains open. Issue a local bearer in Tokens, then use the local `/v1` endpoint in your client. No user/SSH administration is exposed in standalone; the internal owner scope keeps token/usage/affinity semantics consistent.

OAuth add/reauth requires the owner's interactive terminal or TUI Settings browser flow. Never capture real tokens. Enter opens Overview account actions (reset-credit review, prioritize, deactivate/reactivate for the current user), Tokens actions and Settings actions. Account preferences work in remote and standalone modes; operator locks are enforced remotely and cannot be bypassed by CLI or TUI. Deactivation retains OAuth data and can be reversed without login. Higher priority applies only to independent requests, preserving opaque-context and WebSocket ownership. Settings supports account selection, add and reauthorize; real reset credits still require separate confirmation. Operator/server tasks belong to the separate `$exrd` skill.

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

Doctor is a read-only snapshot, not a TLS/upstream/inference probe. Nonzero exit may still include a valid diagnostic JSON report. `exr limits --json` fetches live usage/reset-credit availability and labels accounts by email; run it only within authorized inference scope because of the weekly-activation exception below. A fresh 100%-remaining weekly window with a reset exactly seven days ahead at minute precision may trigger one minimal gpt-5.6-sol request on that enabled account. Attempts are durably limited to once per seven days and never replayed; Doctor, catalog and update checks remain read-only. Window labels: 480-minute windows are 8-hour, 10080-minute windows weekly. Do not infer duration from primary/secondary, sum percentages across subscriptions or treat stale/reset-elapsed values as current availability. Unknown usage is not zero usage; cached/reasoning counters are subsets of input/output.

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

## Reset credits

`exr limits --json` inspects availability without consuming a credit. `exr limits reset <email>` requires an interactive terminal and mandatory human confirmation; there is no `--yes` or JSON bypass. In Overview, select an account with ↑/↓, press Enter and choose Review reset credit (or use `c`); `y` confirms. Never drive confirmation or consume a real credit without the user's explicit approval for that account and credit.

A current subscription window must have ≤5% remaining (≥95% used), with a known future reset. If the free reset is less than three days away, advise waiting as the confirmation does. The server selects the earliest-expiring available supported credit and rechecks usage before consumption. Confirmations expire after two minutes, are user-bound and single-use. Never retry an uncertain reset mutation automatically; read limits first and report the outcome.

## Software updates

Use `exr update --check --json` for a read-only release check. Install only within the user's requested update scope. Built-in update preserves config/state/credentials; reopen the dashboard afterward. Homebrew uses `brew upgrade mocki-toki/exetrouter/exr`; source installs rebuild a locked published tag. Settings offers Check updates and Update exr. Server updates belong to the operator and the separate `exrd` skill.
