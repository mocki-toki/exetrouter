# Client and SDK compatibility

The broader protocol/SDK inference matrix was checked 2026-10-01: Codex CLI 0.159.3 and OpenCode V2 2.0.21, then latest stable versions resolved from npm. The current native HTTP/WS tool and two-compaction/restart matrix was checked 2026-10-02 with Codex CLI 0.160.0 and OpenCode V2 2.0.22. OpenCode uses @opencode/cli; OpenCode V1/opencode-ai and Codex Desktop are outside that full execution matrix. A separate V1 mock check is recorded below.

On 2026-10-02, Codex CLI 0.160.0 was resolved from npm latest and its exact source reviewed at commit `a956835d020762cb2b570053af06f643a11c0ecc`. The configuration below uses its new remote catalog option. Profile loading and authenticated catalog discovery were checked separately against synthetic local metadata, without inference; the broader October 1 protocol/SDK matrix retains its recorded versions.

Latest offline native matrix checked 2026-10-02: Codex 0.160.0 and OpenCode V2 2.0.22 (`527f0b931d1f9b3ebd34e106c51b31ce5db5b075`) passed all four HTTP/WS tool cycles and the Codex HTTP 503 submission check against mock upstream on macOS arm64. Expected test versions now come from the reviewed [source manifest](protocol-sources.json). The [protocol audit](native-protocol-audit.md) compares both clients and OpenCode V1 and records remaining gaps. The Codex fixture also checks native WS interruptions (early close, partial output, no terminal) without replay. OpenCode retains its own retry policy. A completed tool item can execute before the response terminal; these tests do not promise that interruption cancels local tool work. The full live/compaction matrix remains separate. Both reviewed clients also passed all four HTTP/WS quota-switch tool continuations against synthetic pre-generation refusals; see [quota failover evidence](live-testing.md#cross-account-continuity-and-synthetic-quota-failover-2026-10-02).

Additional authorized Linux aarch64 Codex 0.160.0 diagnostics on 2026-10-02 passed native custom-tool cycles through the router after adding the server-owned upstream `thread-id`: a 100-line `apply_patch` over HTTP at about 35k and 120k input tokens, and a Code Mode command over WS at about 120k. All three executed the local tool and reached `response.completed` within 60 seconds. These isolated synthetic tests used loopback ingress, temporary private profiles and disabled retries; they do not establish multi-hour or public-ingress reliability.

After the audit fixes, authorized Linux aarch64 runs also passed current Codex and OpenCode tool cycles over both transports, plus one explicit `zstd` HTTP request. An initial OpenCode WS run exposed a router ordering error: handshake `response.metadata` preceded `response.created`. Sending that notification after creation passed both actual mock clients and a new independent real OpenCode WS cycle (three completed requests, 14 seconds, no HTTP fallback). Codex WS retained an original canary across five process invocations, fresh reference text and a server restart at measured 49–86k input tokens. Two compaction thresholds were lowered during that continuity probe, but actual compaction request counts were not captured; it does not replace the stronger four-scenario compaction harness. See [measured results](live-testing.md#measured-follow-up-2026-10-02).

Both actual binaries passed tool call → local command → tool result → final response over HTTP/SSE and WS against mock and actual ChatGPT upstream. OpenCode uses its standard client retry policy. The router itself never replays an accepted or ambiguous generation.

The stronger October 2 deployment matrix used current clients on macOS arm64 through public HTTPS/WS to a Linux aarch64 router. All four cases passed five process invocations, two separately observed compaction cycles, local tool recall of a canary and verified server restart. Each case retained one owning account; all 13/18/12/12 requests had completed known usage. A separate Codex WS case reached 217,156 observed input tokens and completed compaction/tool continuation with six known requests. Public inference is verified for these bounded scenarios. Full advertised windows, multi-hour sessions, saturation and actual upstream authentication/quota outages remain unverified; see [measured deployment results](live-testing.md#measured-deployment-continuity-2026-10-02). Later versions require exact-source review and rerun, not merely download.

## OpenCode V1 verification

OpenCode V1 1.18.34 passed a synthetic HTTP/SSE Responses tool cycle and continuation after process reopening with the built-in `openai` provider. V1 has an experimental WebSocket transport; its ExetRouter integration, live upstream, compaction and long-context behavior remain unverified. OpenCode manages its own retries; the router does not replay accepted or ambiguous requests.

Reproduce the HTTP fixture with synthetic upstream only:

```sh
python3 scripts/fetch-test-clients.py --opencode-v1 --sources
export EXETROUTER_OPENCODE_V1_BIN="$(python3 -c 'import json; print(json.load(open("target/compat/current-opencode-v1.json"))["binary"])')"
cargo test --locked --test responses opencode_v1_http_compatibility_probe -- --ignored --nocapture
```

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

Four isolated profiles exercise Codex/OpenCode HTTP/WS with two mock accounts. Assertions verify tool execution/result, terminal completion, submissions, usage and primary transport. Codex WS warmup and OpenCode's auxiliary HTTP title are accounted separately. Mock HTTP 503 verifies one main submission; not all client recovery paths are covered.

## SDK fixtures

Verified stable SDKs: Python openai 3.22.1 and JavaScript openai 7.25.0. Both passed models, Responses JSON/SSE, Chat JSON/SSE, function cycles, unsupported-parameter 400 and stream APIError against mock upstream.

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

Offline Rust unit tests cover Chat user text/image translation, preserving multiple-image order and detail. Validation tests reject invalid image roles/forms. Image-specific JSON/SSE integration, inputs larger than 1 MiB and rejection before upstream submission still need verification. This is adapter evidence, separate from the SDK/native tool-cycle matrix above. Live image understanding, model-specific detail support and native multimodal tool results remain unverified.

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
request_max_retries = 0
stream_max_retries = 0
```

Replace both URLs with your router's address, then use `codex --profile exetrouter`. The discovery feature is off by default and marked under development in 0.160.0; both it and `model_catalog_url` are needed for live metadata with this bearer-authenticated custom provider. The catalog request uses the router token and does not submit inference. Codex receives the router's account-visible model IDs, context limits, reasoning options and modalities. No fixed `model` setting is required: the profile inherits a configured preference or uses a catalog default. An inherited preference must exist in the pool; `/model` selects another available model. Model availability is not identical across every ChatGPT account or the OpenAI Platform API.

Provider name `OpenAI` selects remote compaction V2; provider ID/base URL/bearer still belong to the router. Do not copy Codex `auth.json` to the server or enable `requires_openai_auth`. An isolated `supports_websockets=false` profile tests HTTP/SSE.

To migrate the old README configuration, move its ExetRouter settings into the separate profile file. Remove `[profiles.exetrouter]` and any top-level `profile = "exetrouter"` from the main `config.toml`; remove the ExetRouter `model_catalog_json` override to enable remote discovery. Preserve unrelated configuration. See [official profile migration](https://developers.openai.com/codex/config-advanced#profiles).

For older clients without `model_catalog_url`, an optional snapshot remains available: export `exr models --json --format codex-json` to a user-selected file and add `model_catalog_json = "/absolute/path/exetrouter-models.json"` at the profile's top level, before table headers. Omit the new catalog URL/discovery settings and refresh the snapshot after pool changes. Without either catalog source, the custom provider uses bundled/fallback metadata, which may differ from the pool.

## OpenCode setup

Use ExetRouter as your OpenAI endpoint. No additional plugin is needed.

Run `opencode --version` and use the export format for your installed major version. Set `EXETROUTER_TOKEN` to your router token in the launching environment. The generated configuration keeps an environment reference, never the token itself. Standalone exports use the running/configured local API address automatically. For a remote connection, save the operator’s public HTTP API URL once (it may differ from the SSH host):

```sh
exr configure --api-url https://api.example.com/v1
```

Exports reuse this setting; `--base-url URL` is an optional override for a single export. In standalone mode keep `exr` or `exr serve` running while OpenCode uses its API.

### V1 (opencode-ai)

```sh
exr models --json --format opencode-v1-json > exetrouter-v1.json &&
  OPENCODE_CONFIG="$PWD/exetrouter-v1.json" opencode --model exetrouter/gpt-5.6-sol
```

The export defines `provider.exetrouter` with `"npm": "@ai-sdk/openai"`, `"name": "ExetRouter"`, connection options and models extracted from your account pool. Model limits, modalities, options and reasoning variants use V1's native configuration format.

### V2 (@opencode/cli)

```sh
exr models --json --format opencode-v2-json --model exetrouter/gpt-5.6-sol > exetrouter-v2.json &&
  OPENCODE_CONFIG="$PWD/exetrouter-v2.json" opencode --standalone
```

The export defines `providers.exetrouter` with `"name": "ExetRouter"`, package `@opencode/ai/providers/openai/responses`, the token environment variable, connection settings and models extracted from your account pool. It uses WebSocket transport, `store=false` and native compaction. `--standalone` starts a private OpenCode server with the fresh configuration instead of reusing an existing background server. For HTTP/SSE, change `providers.exetrouter.settings.transport` to `http`. Model capabilities, settings and reasoning variants use V2's native format.

Both commands query ExetRouter before launching OpenCode and import the generated file through OpenCode's native `OPENCODE_CONFIG` setting. `exetrouter/gpt-5.6-sol` means provider `exetrouter`, model ID `gpt-5.6-sol`. The examples select this model: V1 through the launch flag, V2 through the exported configuration. It must be available in `exr models`; exporting with `--model` rejects an unavailable ID. Choose a different imported model through `/models`.

Without a model override, OpenCode keeps its own selection rules. V1 uses a configured model, then an available recent model, then its default ranking. V2 uses a configured/default model, otherwise an available catalog model; the interactive client applies its own selection preferences. Neither mode guarantees ExetRouter will be selected if other providers are available. Omitting the exporter’s `--model` leaves the exported config’s `model` unset. V2 also accepts `opencode run --standalone --model exetrouter/gpt-5.6-sol` for a noninteractive run.

The dedicated `exetrouter` provider gets its model list and metadata from the extractor, without inheriting OpenCode's bundled OpenAI catalog. Other providers and unrelated settings can stay in your regular OpenCode configuration; keep the `exetrouter` definition in the generated file so older local model entries do not merge into it. Run the matching export again before each launch to refresh pool/catalog changes. No conversion script or plugin is required. OpenCode retains its standard retry policy. See [example output for every format](model-export-examples.md).

### OpenCode model import

Export shapes were checked against V1 1.18.34 and V2 2.0.22 configuration sources on 2026-10-04. Offline CLI/API tests check the extracted metadata and connection configuration; isolated native model-list checks, after catalog initialization, verify both versions load the generated files and expose only the exported IDs under `exetrouter`. Native mock tool-cycle checks also passed with these exported providers: V1 HTTP/SSE and process resume, and V2 HTTP/SSE and WebSocket. These use a synthetic loopback backend; they do not renew the real upstream inference matrix above. Unknown output limits remain zero under OpenCode's convention.

Authenticated HTTP clients can obtain model/provider fragments at `/v1/models/opencode-v1` and `/v1/models/opencode-v2`. Those fragments omit `baseURL`; the CLI adds the saved/standalone API URL to produce a configuration ready for file import. `baseURL` alone changes the request destination and does not fetch models from the router.
