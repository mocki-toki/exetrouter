# Connect a client

Use the endpoint shown in standalone Settings (default `http://127.0.0.1:8787/v1`) or the operator's verified HTTPS URL. Keep the standalone dashboard or `exr serve` running. Discover available models with `exr models --json`.

Supply the router bearer through the consuming process's secret environment. Its API-key field takes this bearer, not a ChatGPT OAuth or Platform key. Never put the secret in generated configuration, arguments or logs.

## Codex CLI

For Codex 0.160.0+, save `~/.codex/exetrouter.config.toml` with top-level settings:

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

Replace both URLs and launch `codex --profile exetrouter` with `EXETROUTER_TOKEN` available. Discovery fetches metadata without inference; it is opt-in and under development in 0.160.0. `/model` selects a pool model. An inherited preference must exist in the pool. Provider name `OpenAI` enables native remote compaction V2.

Preserve unrelated settings. Move legacy `[profiles.exetrouter]` and top-level `profile="exetrouter"` from the main config into the separate profile file; remove the ExetRouter `model_catalog_json` override when using discovery. Never copy Codex `auth.json` into router state. Check the installed version's profile format before editing.

Older clients may use a snapshot from `exr models --json --format codex-json`, with an absolute `model_catalog_json` path at the profile's top level. Omit unsupported URL/discovery fields and regenerate after pool changes.

## OpenCode V1 and V2

Check `opencode --version` and set `EXETROUTER_TOKEN`. Exports reuse the HTTP API URL saved in the wizard or Settings; standalone derives its local address. If the optional remote URL was skipped, set it in Settings or with `exr configure --api-url URL`. `--base-url` overrides one export.

For V1:

Create the configuration:

```sh
exr models --json --format opencode-v1-json > exetrouter-v1.json
```

Then start OpenCode:

```sh
OPENCODE_CONFIG="$PWD/exetrouter-v1.json" opencode --model exetrouter/gpt-5.6-sol
```

For V2:

Create the configuration:

```sh
exr models --json --format opencode-v2-json --model exetrouter/gpt-5.6-sol > exetrouter-v2.json
```

Then start OpenCode:

```sh
OPENCODE_CONFIG="$PWD/exetrouter-v2.json" opencode --standalone
```

Run from the configuration file’s directory, or use its absolute path in `OPENCODE_CONFIG`.

The example model must appear in `exr models`; `exetrouter/gpt-5.6-sol` is provider/model notation. Without a model override, OpenCode may select another provider. Choose **ExetRouter** through `/models`.

Both exports import pool metadata under a dedicated `exetrouter` provider without inheriting the bundled OpenAI catalog. V1 uses `provider`, `@ai-sdk/openai`, options and variant objects. V2 uses `providers`, `@opencode/ai/providers/openai/responses`, settings and variant arrays. V2 defaults to WS, `store=false` and native compaction; set `providers.exetrouter.settings.transport` to `http` for SSE. `--standalone` makes a private OpenCode server read the fresh file.

Regenerate the configuration after account-pool or model changes. Keep this provider's definition in the generated file to avoid merging old model entries; unrelated settings stay in regular configuration. OpenCode keeps its standard retries. Exported providers passed mock model/tool checks in V1 1.18.34 and V2 2.0.22. V1 WS, live upstream, live compaction and long context remain unverified; repository `docs/compatibility.md` records the full evidence.

For V1 plaintext compaction with retained history, install the repository's `clients/opencode-v1/context.js` in that V1 configuration's `plugins/` directory, then reopen the existing session. It removes only the compaction summary's ExetRouter-enveloped hidden reasoning from outgoing copies; the visible summary, tail messages/tools and transfer receipts remain unchanged. This prevents independent summary routing from causing `context_account_mismatch` and was reproduced/recovered offline on V1 1.18.30. Do not delete history or relax server ownership checks. Codex/V2 native checkpoints do not need this plugin.

## MCP image tools

Use the API origin's `/mcp` endpoint, not `/v1/mcp`, with the existing router bearer. Codex: `[mcp_servers.exetrouter]` with `url = "https://api.example.com/mcp"`, `bearer_token_env_var = "EXETROUTER_TOKEN"` and `tool_timeout_sec = 900`. OpenCode V2: an entry under `mcp.servers.exetrouter` with `type: "remote"`, the `/mcp` URL, `oauth: false`, `headers: {"Authorization": "Bearer {env:EXETROUTER_TOKEN}"}`, `codemode: false` and `timeout: {"execution": 900000}`. Preserve other configuration and do not embed secrets. OpenCode reloads configuration automatically; inspect `/mcps` or `opencode mcp list`.

The agent discovers `generate_image` and `edit_image` with schemas/instructions. Results contain image content for the client to display/save; server-local paths are never accepted. Editing needs inline data-image/HTTPS references. Tools consume image quota and must not be retried blindly after interruption. No skill/helper or built-in Codex image extension is required. Clients can cache tool definitions until refresh/reconnect. Repository `docs/mcp.md` records protocol/client scope and deployment requirements.

## SDKs and service APIs

Python: `OpenAI(base_url=URL, api_key=os.environ["EXETROUTER_TOKEN"])`. JavaScript: `new OpenAI({baseURL: URL, apiKey: process.env.EXETROUTER_TOKEN})`.

Supported calls are models, Responses JSON/SSE/WS/compact, limited Chat with function tools, and the documented native JSON image/search subset on schema-13 servers. Use full history and `store=false`; HTTP `previous_response_id` is unsupported. Backend validates output caps and model options. Image input passes through Responses `input_image`, or ordered Chat user `image_url` parts; offline forwarding evidence does not verify live vision. Use the bundled image helper for shell agents; image generation does not require a conversational host model.

Normal opaque-context/WS ownership stays on the original account; quota-only transfer requires complete current context. Native standalone-search sessions cannot migrate backend-held references. Embeddings use a configured Platform project and are listed in `/v1/models` with the `embeddings` capability. Audio/files/batches are absent. Native Codex image-tool activation still requires client capability/auth support; API route availability alone does not enable it. Check repository `docs/openai-api.md` and `docs/native-tools.md` before adding an integration.

Server user/key/OAuth/backup operations belong to `exrd`, under the separate server skill. They are unavailable through remote `exr`; standalone Settings manages only its local accounts. Device login requires the owner's browser interaction, and credentials must never be requested in chat.
