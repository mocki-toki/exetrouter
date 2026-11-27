# Client setup and compatibility

Supply your router endpoint, bearer token and an account-visible model. The setup below covers Codex CLI, OpenCode V1/V2 and OpenAI SDKs; the router's exact request contract is in [openai-api.md](openai-api.md).

## Verified versions and scope

| Checked | Clients | Evidence |
| --- | --- | --- |
| October 1, 2026 | Codex 0.159.3, OpenCode V2 2.0.21; Python openai 3.22.1, JavaScript openai 7.25.0 | Broader protocol/SDK matrix; SDK runs use mock upstream. |
| October 2, 2026 | Codex 0.160.0, OpenCode V2 2.0.22 | Mock and real HTTP/WS tool cycles; public-ingress two-compaction/reopen/restart matrix. |
| October 4, 2026 | OpenCode V1 1.18.34, V2 2.0.22 | Exported-provider model lists and mock tool cycles; V1 HTTP/SSE plus resume, V2 HTTP/SSE and WS. |
| October 5, 2026 | Codex 0.160.0, OpenCode V2 2.0.23 | Synthetic HTTP/WS tool cycles, remote compaction, client reopen and router restart with client-carried ownership proofs; quota-switch continuations. |
| October 24, 2026 | Codex 0.164.0, OpenCode V2 2.1.4 | Targeted synthetic quota transfer with encrypted history, client-carried portability receipt, router restart, process reopen and model switch/back on HTTP/WS. Not a renewal of the full upstream matrix. |
| October 29, 2026 | OpenCode V1 1.18.30 | Reproduced plaintext-compaction/retained-tail account conflict; bundled request-copy plugin recovers the same saved session across router/client restart. Synthetic HTTP/SSE only. |

The [source manifest](protocol-sources.json) pins reviewed revisions. The [protocol audit](native-protocol-audit.md) records source-level contracts; [live-testing.md](live-testing.md) records measured runs, failed probes and request counts. Later versions need source review and new fixtures.

Codex WS interruption fixtures counted one generation for early close, partial output and missing terminal. Both current clients passed synthetic quota-switch continuations. A completed tool item may run before the response terminal, and a lost connection may prevent delivery of a terminal error.

Full advertised context windows, multi-hour sessions, saturation and actual upstream authentication/quota outages remain unverified. OpenCode V1's experimental WS transport, live upstream, live compaction and long context, and Codex Desktop are outside the verified execution matrix.

## Configure your client

Run `exr` and complete the first-run wizard (or edit the connection in Settings). Create a token in Tokens, paste its clipboard secret into your secret store and supply it as EXETROUTER_TOKEN to the consuming process. Pick a model in Models; Enter copies its ID. Do not put bearer secrets into config files, repository, command arguments or shell history. Rotation immediately revokes the previous token.

The examples use a reserved example domain. Replace it with the service endpoint supplied by your operator.

### Codex CLI

For Codex CLI 0.160.0 or newer, create `~/.codex/exetrouter.config.toml`. Profile files contain top-level settings, not `[profiles.exetrouter]` tables:

```toml
model_provider = "exetrouter"

[features]
api_key_model_discovery = true

[model_providers.exetrouter]
name = "OpenAI"
base_url = "https://api.example.com/v1"
model_catalog_url = "https://api.example.com/v1/models/codex"
wire_api = "responses"
env_key = "EXETROUTER_TOKEN"
supports_websockets = true
```

Replace both URLs with your router's address, then use `codex --profile exetrouter`. The discovery feature is off by default and marked under development in 0.160.0; both it and `model_catalog_url` are needed for live metadata with this bearer-authenticated custom provider. The catalog request uses the router token and does not submit inference. Codex receives the router's account-visible model IDs, context limits, reasoning options and modalities. No fixed `model` setting is required: the profile inherits a configured preference or uses a catalog default. An inherited preference must exist in the pool; `/model` selects another available model. Model availability is not identical across every ChatGPT account or the OpenAI Platform API.

Provider name `OpenAI` selects remote compaction V2; provider ID/base URL/bearer still belong to the router. Do not copy Codex `auth.json` to the server or enable `requires_openai_auth`. An isolated `supports_websockets=false` profile tests HTTP/SSE.

To migrate the old README configuration, move its ExetRouter settings into the separate profile file. Remove `[profiles.exetrouter]` and any top-level `profile = "exetrouter"` from the main `config.toml`; remove the ExetRouter `model_catalog_json` override to enable remote discovery. Preserve unrelated configuration. See [official profile migration](https://developers.openai.com/codex/config-advanced#profiles).

For older clients without `model_catalog_url`, an optional snapshot remains available: export `exr models --json --format codex-json` to a user-selected file and add `model_catalog_json = "/absolute/path/exetrouter-models.json"` at the profile's top level, before table headers. Omit the new catalog URL/discovery settings and refresh the snapshot after pool changes. Without either catalog source, the custom provider uses bundled/fallback metadata, which may differ from the pool.

## OpenCode setup

Use ExetRouter as your OpenAI endpoint. No additional plugin is needed.

Check `opencode --version` and export the format for your installed major version. Supply `EXETROUTER_TOKEN` in the launching environment; the generated file contains an environment reference, never the secret.

Exports use the API URL saved in the first-run wizard or Settings. Standalone derives its local address automatically; keep `exr` or `exr serve` running. If you skipped the optional remote HTTP API URL during setup, add it in Settings or run:

```sh
exr configure --api-url https://api.example.com/v1
```

The API URL may differ from the SSH host. `--base-url URL` overrides it for one export.

### V1 (opencode-ai)

Create the configuration:

```sh
exr models --json --format opencode-v1-json > exetrouter-v1.json
```

Then start OpenCode:

```sh
OPENCODE_CONFIG="$PWD/exetrouter-v1.json" opencode --model exetrouter/gpt-5.6-sol
```

V1's plaintext compaction can combine an independent summary and a retained tail
from different pool accounts. Install the bundled
[V1 context plugin](../clients/opencode-v1/README.md) on that client before using
long sessions. It omits only the summary's router-enveloped hidden reasoning
from outgoing copies; summary text, original history and transfer proofs remain.
It can recover an already affected session without editing its database. V2 and
Codex native compaction do not use this plugin.

### V2 (@opencode/cli)

Create the configuration:

```sh
exr models --json --format opencode-v2-json --model exetrouter/gpt-5.6-sol > exetrouter-v2.json
```

Then start OpenCode:

```sh
OPENCODE_CONFIG="$PWD/exetrouter-v2.json" opencode --standalone
```

Run from the configuration file’s directory, or use its absolute path in `OPENCODE_CONFIG`.

Both exports create a dedicated provider named **ExetRouter**, with models and metadata from your account pool. V1 uses `@ai-sdk/openai`; V2 uses `@opencode/ai/providers/openai/responses`, WebSocket, `store=false` and portable summary compaction. For V2 HTTP/SSE, set `providers.exetrouter.settings.transport` to `http`. `--standalone` starts a private OpenCode server that reads the fresh configuration.

`exetrouter/gpt-5.6-sol` means provider/model. The model must appear in `exr models`; `--model` rejects unavailable IDs. Choose another imported model through `/models`. Without a model override, OpenCode follows its configured/recent/default preferences and may select another provider. Omitting the exporter's `--model` leaves the top-level `model` unset. V2 also supports `opencode run --standalone --model exetrouter/gpt-5.6-sol`.

Regenerate the configuration after account-pool or model changes. Keep the `exetrouter` provider definition in the generated file so old entries do not merge into it; unrelated providers and settings can stay in your regular configuration. The export does not inherit OpenCode's bundled OpenAI catalog. OpenCode retains its standard retry policy. See [complete output examples](model-export-examples.md).

### OpenCode model import

Export shapes were checked against V1 1.18.34 and V2 2.0.22 configuration sources on 2026-10-04. Offline CLI/API tests check the extracted metadata and connection configuration; isolated native model-list checks, after catalog initialization, verify both versions load the generated files and expose only the exported IDs under `exetrouter`. Native mock tool-cycle checks also passed with these exported providers: V1 HTTP/SSE and process resume, and V2 HTTP/SSE and WebSocket. These use a synthetic loopback backend; they do not renew the real upstream inference matrix above. Unknown output limits remain zero under OpenCode's convention.

Authenticated HTTP clients can obtain model/provider fragments at `/v1/models/opencode-v1` and `/v1/models/opencode-v2`. Those fragments omit `baseURL`; the CLI adds the saved/standalone API URL to produce a configuration ready for file import. `baseURL` alone changes the request destination and does not fetch models from the router.

## OpenCode V1 verification

To reproduce the synthetic HTTP/SSE tool and process-resume fixture:

```sh
python3 scripts/fetch-test-clients.py --opencode-v1 --sources
export EXETROUTER_OPENCODE_V1_BIN="$(python3 -c 'import json; print(json.load(open("target/compat/current-opencode-v1.json"))["binary"])')"
cargo test --locked --test responses opencode_v1_http_compatibility_probe -- --ignored --nocapture
```

The targeted compaction fixture pins V1 **1.18.30**
(`3104c1428ec91f809e5ab86631300de41eb6952e`), whose reviewed
`session/compaction.ts` serializes the summary input as plaintext, retains a tail
and then replays the summary's reasoning through `session/message-v2.ts`.
Current stable V1 resolved to **1.19.5**
(`810c0eba1a7348036f8bdf61e6632d0c36c6876c`); its message/processor source review
is not a native compaction execution claim for that version.

`opencode_v1_compaction_preserves_tail_across_independent_summary_account`
reproduces the unmodified 400 before inference, installs the request-copy plugin
into the isolated client profile, and reopens that same saved session after a
router restart. It checks exact original tail reasoning/tool results, omission
of only the summary reasoning, and preservation of new ordinary reasoning and
transfer receipts on another reopen. Four synthetic admissions complete; no
real account, inference or reset credit is used. Run the plugin's dependency-free
unit tests with `node --test clients/opencode-v1/context.test.js`.

## Resolve and run native fixtures

```sh
python3 scripts/check-protocol-sources.py --fetch --check-current
python3 scripts/fetch-test-clients.py --resolve-only
python3 scripts/fetch-test-clients.py --sources
```

The loader verifies official npm SHA-512 registry integrity, resolves exact release-tag commits and stores binaries/source/manifests in ignored target/compat. Codex's full runtime, including codex-code-mode-host, is retained. Global installations and normal auth/config are unchanged. Download targets support macOS/Linux arm64/x64; Linux aarch64 native execution is verified against mock upstream; real-upstream Linux testing is separate.

```sh
python3 - <<'PY'
import json, os, subprocess
from pathlib import Path
clients = json.loads(Path("target/compat/current-clients.json").read_text())
env = dict(os.environ)
env["EXETROUTER_CODEX_BIN"] = clients["codex"]["binary"]
env["EXETROUTER_OPENCODE_BIN"] = clients["opencode"]["binary"]
subprocess.run(["cargo", "test", "--locked", "--test", "responses",
    "current_clients_complete_tool_cycles", "--", "--ignored", "--nocapture"],
    env=env, check=True)
PY
```

Four isolated profiles exercise Codex/OpenCode HTTP/WS with two mock accounts. Assertions verify tool execution/result, terminal completion, submissions, usage and primary transport. The proof fixture forces synthetic compaction, restarts the router and resumes each client process; encrypted context reaches the mock upstream without the router envelope, and normal continuations create no context-binding rows. Codex WS warmup and OpenCode's auxiliary HTTP title are accounted separately. Mock HTTP 503 verifies one main submission; not all client recovery paths are covered.

### Stateless transfer receipt follow-up

The October 24 targeted check resolved Codex **0.164.0** (`6aaac694803004a63c6f393d153c96d22521a6b9`) and OpenCode **2.1.4** (`68bc26b7da1990ce10ffecfbc1e09eb13aa5d824`) from stable npm releases and exact release tags. Source review covered Codex `protocol/src/models.rs`, `core/src/client.rs` and history retention, plus OpenCode `packages/ai/src/protocols/open-responses.ts`, `open-responses-continuation.ts` and the session message lowering. Empty encrypted reasoning is retained as opaque history. Codex reserializes absent reasoning content as `null`; the router accepts that empty representation without allowing visible carrier content.

`native_clients_retain_stateless_transfer_receipts_after_router_and_process_restart` pins those versions explicitly. Four isolated mock cases passed: HTTP and WS for each client. Each executes a real local synthetic tool, receives a pre-generation quota refusal, transfers its encrypted history, restarts the router and resumes the saved client process, then switches to a second model and back. The backend receives original ancestor values, never `exrtr1` carriers; a trigger forbids context inserts and the registry remains empty. OpenCode's existing `to-llm-message.ts` policy omits model-bound encrypted reasoning when the selected model differs, then reuses it on returning to its producing model; the fixture checks tool-result history throughout and encrypted history again on return. The router does not change that client policy or bind transfer receipts to a model. The cases recorded respectively 6/10 Codex and 7/7 OpenCode requests, including warmup/auxiliary operations. No real OAuth account, inference or reset credit is used.

Ordinary offline fixtures separately cover eight successive transfers per JSON/SSE/WS case with model changes, a restart after each transfer and zero registry growth, receipt tampering/foreign-user/unrelated-context rejection, the exact 16,384-element snapshot bound, a full legacy registry and compaction replacement. These are not live multi-hour or native-client compaction-after-transfer acceptance claims. A requested model must still be available on the owning account; these checks do not authorize moving history solely to bypass an account's missing model or backoff. Explicit deactivation permits a full-context transfer to an eligible active account under the [account-pool contract](account-pool.md#availability-and-automatic-selection).

With those exact isolated runtimes in `EXETROUTER_CODEX_BIN` and `EXETROUTER_OPENCODE_BIN`, run:

```sh
cargo test --locked --test responses \
  native_clients_retain_stateless_transfer_receipts_after_router_and_process_restart \
  -- --ignored --nocapture
```

## SDK fixtures

SDKs checked on October 1, 2026: Python openai 3.22.1 and JavaScript openai 7.25.0. Both passed models, Responses JSON/SSE, Chat JSON/SSE, function cycles, unsupported-parameter 400 and stream APIError against mock upstream.

```sh
python3 -m venv target/compat/openai-python
target/compat/openai-python/bin/python -m pip --isolated install openai==3.22.1
npm install --prefix target/compat/openai-js \
  --registry https://registry.npmjs.org --userconfig /dev/null \
  --globalconfig "$PWD/target/compat/npm-global.config" \
  --cache target/compat/npm-cache --ignore-scripts --no-audit --no-fund openai@7.25.0
EXETROUTER_PYTHON_BIN="$PWD/target/compat/openai-python/bin/python" \
EXETROUTER_NODE_BIN="$(command -v node)" \
EXETROUTER_OPENAI_JS_MODULE="$PWD/target/compat/openai-js/node_modules/openai/index.mjs" \
cargo test --locked --test responses openai_sdks_complete_json_stream_and_tool_cycles \
  -- --ignored --nocapture
```

Node 22+ is required; the temporary npm globalconfig must be absent/empty. Fixture credentials are synthetic. Real tests are separate.

## Image forwarding fixtures

Offline unit and integration fixtures cover ordered Chat text/image translation, optional detail, inputs larger than 1 MiB, JSON/SSE over HTTP/WS upstream transports, rejection before submission/accounting and payload privacy. Native Responses image/opaque-item ordering also has fixtures. This verifies forwarding; live image understanding, model-specific detail support and native multimodal tool results remain unverified.
