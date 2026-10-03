# Client and SDK compatibility

The broader protocol/SDK inference matrix was checked 2026-10-01: Codex CLI 0.159.3 and OpenCode V2 2.0.21, then latest stable versions resolved from npm. The current native HTTP/WS tool and two-compaction/restart matrix was checked 2026-10-02 with Codex CLI 0.160.0 and OpenCode V2 2.0.22. OpenCode uses @opencode/cli; OpenCode V1/opencode-ai and Codex Desktop are outside that full execution matrix. A separate V1 mock check is recorded below.

On 2026-10-02, Codex CLI 0.160.0 was resolved from npm latest and its exact source reviewed at commit `a956835d020762cb2b570053af06f643a11c0ecc`. The configuration below uses its new remote catalog option. Profile loading and authenticated catalog discovery were checked separately against synthetic local metadata, without inference; the broader October 1 protocol/SDK matrix retains its recorded versions.

Latest offline native matrix checked 2026-10-02: Codex 0.160.0 and OpenCode V2 2.0.22 (`527f0b931d1f9b3ebd34e106c51b31ce5db5b075`) passed all four HTTP/WS tool cycles and both HTTP 503 submission checks against mock upstream on macOS arm64. Expected test versions now come from the reviewed [source manifest](protocol-sources.json). The [protocol audit](native-protocol-audit.md) compares both clients and OpenCode V1 and records remaining gaps. Current Codex/OpenCode also passed six native WS interruption cases (early close, partial output, no terminal), with one submitted generation each and no HTTP replay. A completed tool item can execute before the response terminal; these tests do not promise that interruption cancels local tool work. The full live/compaction matrix remains separate. Both reviewed clients also passed all four HTTP/WS quota-switch tool continuations against synthetic pre-generation refusals; see [quota failover evidence](live-testing.md#cross-account-continuity-and-synthetic-quota-failover-2026-10-02).

Additional authorized Linux aarch64 Codex 0.160.0 diagnostics on 2026-10-02 passed native custom-tool cycles through the router after adding the server-owned upstream `thread-id`: a 100-line `apply_patch` over HTTP at about 35k and 120k input tokens, and a Code Mode command over WS at about 120k. All three executed the local tool and reached `response.completed` within 60 seconds. These isolated synthetic tests used loopback ingress, temporary private profiles and disabled retries; they do not establish multi-hour or public-ingress reliability.

After the audit fixes, authorized Linux aarch64 runs also passed current Codex and OpenCode tool cycles over both transports, plus one explicit `zstd` HTTP request. An initial OpenCode WS run exposed a router ordering error: handshake `response.metadata` preceded `response.created`. Sending that notification after creation passed both actual mock clients and a new independent real OpenCode WS cycle (three completed requests, 14 seconds, no HTTP fallback). Codex WS retained an original canary across five process invocations, fresh reference text and a server restart at measured 49–86k input tokens. Two compaction thresholds were lowered during that continuity probe, but actual compaction request counts were not captured; it does not replace the stronger four-scenario compaction harness. See [measured results](live-testing.md#measured-follow-up-2026-10-02).

Both actual binaries passed tool call → local command → tool result → final response over HTTP/SSE and WS against mock and actual ChatGPT upstream. The same four mock native cycles and HTTP 503 no-replay checks passed on Raspberry Pi/Debian aarch64 on 2026-10-01.

The stronger October 2 deployment matrix used current clients on macOS arm64 through public HTTPS/WS to a Linux aarch64 router. All four cases passed five process invocations, two separately observed compaction cycles, local tool recall of a canary and verified server restart. Each case retained one owning account; all 13/18/12/12 requests had completed known usage. A separate Codex WS case reached 217,156 observed input tokens and completed compaction/tool continuation with six known requests. Public inference is verified for these bounded scenarios. Full advertised windows, multi-hour sessions, saturation and actual upstream authentication/quota outages remain unverified; see [measured deployment results](live-testing.md#measured-deployment-continuity-2026-10-02). Later versions require exact-source review and rerun, not merely download.

## OpenCode V1 check (2026-10-03)

Official npm `opencode-ai` latest resolved to **1.18.34**, tag `v1.18.34`, commit `aec0b9a6d8898f68f923aaf08b7306d931fd9d76`. The macOS arm64 binary passed npm SHA-512 integrity verification. Its exact provider, parameter preparation, processor and retry sources are recorded in [the source manifest](protocol-sources.json).

An isolated profile passed HTTP/SSE Responses → `bash` tool execution → tool result → final response, then a second process continued the persisted session successfully. The fixture used provider ID `openai`, bundled `@ai-sdk/openai`, a router `/v1` base URL, `EXETROUTER_TOKEN`, model `options.store=false` and explicit model limits. V1 uses singular `provider`, `npm` and `options`; the V2 configuration and bridge below do not apply. V1's bundled OpenAI plugin removes the output cap for provider ID `openai`; an arbitrary custom provider ID is not covered by this check.

The unmodified V1 client repeated the primary request **six times** when the mock router/upstream returned HTTP 503. A second synthetic scenario disconnected after `response.created`; V1 reached the probe budget of **12 submitted primary requests**, with no terminal usage samples. Its session processor retries independently of the AI SDK's `maxRetries: 0`, and the reviewed public plugin/config surface has no equivalent to V2's retry decision hook. Therefore the unmodified V1 client has **partial protocol compatibility, not supported production compatibility** under ExetRouter's no-replay contract. Do not infer live reliability from the successful tool cycle. Real upstream, WebSocket, compaction, large context and server restart were not exercised for V1.

The provider-scoped [V1 bridge](../clients/opencode-v1/exetrouter.mjs) was then checked with the same official binary. It passed HTTP/SSE tool execution and process reopening, plus HTTP 503, early EOF, disconnect after creation and missing terminal: **one primary submission and one visible error in each failure case**. Partial text may already have appeared before the error. Ordinary Node fixtures additionally cover network rejection, explicit SSE failures, malformed/oversized events, header/idle deadlines, cancellation and provider isolation. This verifies these mock scenarios, not live upstream or every recovery mechanism.

V1 has no public session retry decision hook. The bridge uses its public `config` hook to wrap only `provider.exetrouter.options.fetch`, disable V1's outer retryable timeout wrappers and apply its own deadlines. It preserves successful SSE events; failures become a fixed client Error with no original payload, HTTP status or nested cause for V1's retry classifier to match. The detailed upstream error is intentionally not rendered. Header and idle deadlines default to five minutes; positive numeric provider deadline settings are retained. SSE input chunks/pending frames are bounded to 1 MiB and one event is emitted per pull. It supports HTTP/SSE Responses only; V1's summarizing compaction, WebSocket and JSON generation are outside this bridge's verified contract.

Reproduce with synthetic upstream only:

```sh
python3 scripts/fetch-test-clients.py --opencode-v1 --sources
export EXETROUTER_OPENCODE_V1_BIN="$(python3 -c 'import json; print(json.load(open("target/compat/current-opencode-v1.json"))["binary"])')"
cargo test --locked --test responses opencode_v1_http_compatibility_probe -- --ignored --nocapture
cargo test --locked --test responses opencode_v1_bridge -- --ignored --nocapture
# Optional: measure the unmodified client
cargo test --locked --test responses opencode_v1_error_retry_probe -- --ignored --nocapture
node --test tests/support/opencode-v1-bridge.test.mjs
```

The error probe records the known retry limitation rather than accepting V1 into the supported matrix. These tests do not change normal installations, profiles, credentials or the deployed server.

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

### OpenCode V1 (1.18.34, HTTP/SSE bridge)

Use the separate [V1 bridge](../clients/opencode-v1/exetrouter.mjs); its measured scope and limitations are [above](#opencode-v1-check-2026-10-03). Merge into the chosen `opencode.jsonc`, preserving unrelated configuration:

```jsonc
{
  "plugin": ["file://<ABSOLUTE_REPOSITORY_PATH>/clients/opencode-v1/exetrouter.mjs"],
  "provider": {
    "exetrouter": {
      "name": "ExetRouter",
      "npm": "@ai-sdk/openai",
      "env": ["EXETROUTER_TOKEN"],
      "options": {"baseURL": "https://api.example.com/v1"},
      "models": {}
    }
  }
}
```

V1 requires its own model descriptors. Export the real catalog, then convert it without inventing limits:

```sh
exr models --json --format opencode-jsonc > exetrouter-models.json
python3 - <<'PYMODELS'
import json
models = json.load(open("exetrouter-models.json"))["providers"]["exetrouter"]["models"]
result = {key: {"name": model["name"], "limit": model["limit"],
    "tool_call": model["capabilities"]["tools"],
    "modalities": {"input": model["capabilities"]["input"], "output": model["capabilities"]["output"]},
    "reasoning": bool(model["variants"]), "options": {**model["settings"], "store": False},
    "variants": {variant["id"]: variant["settings"] for variant in model["variants"]}}
    for key, model in models.items()}
print(json.dumps(result, indent=2))
PYMODELS
```

Paste that object into `provider.exetrouter.models` and select `exetrouter/MODEL_ID`. The bridge removes unsupported output caps/sampling and sets `store=false` for this provider. Keep the bearer in the environment. Do not enable V1's experimental WebSocket/native LLM paths: they have not been tested with this bridge. Recheck exact sources and native fixtures before claiming support for another V1 version.

### OpenCode V2

Merge into the selected opencode.jsonc, preserving unrelated providers/plugins:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": ["<ABSOLUTE_REPOSITORY_PATH>/clients/opencode/exetrouter"],
  "providers": {
    "exetrouter": {
      "name": "ExetRouter",
      "canonical": "openai",
      "package": "@opencode/ai/providers/openai/responses",
      "env": ["EXETROUTER_TOKEN"],
      "settings": {
        "baseURL": "https://api.example.com/v1",
        "transport": "websocket",
        "store": false,
        "compaction": {"type": "native"}
      },
      "models": {"<MODEL_ID_FROM_EXR_MODELS>": {}}
    }
  }
}
```

Replace model descriptors with providers.exetrouter.models from exr models --json --format opencode-jsonc. Select exetrouter/MODEL_ID. The repository [plugin](../clients/opencode/exetrouter/index.js) removes automatic output caps and rejects this provider's session retry hook. It does not cover every client recovery mechanism. Use transport=http for isolated HTTP/SSE testing. Actual context/input limits and reasoning variants come from the catalog, not guessed constants.

Doctor only reports stored metadata. Actual transport and tool execution require the separate fixture; [live tests](live-testing.md) document its measured scope.
