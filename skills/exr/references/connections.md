# Client connections and operator boundary

Use a model ID discovered with `exr models --json`. Obtain the service base URL from the deployment; `https://api.example.com/v1` is a reserved example, while local tests use loopback. Verify the actual service endpoint and TLS certificate.

The library's API-key field holds an **ExetRouter bearer**, supplied to the consuming process through its secret environment. Upstream uses ChatGPT OAuth, not Platform API-key BYOK. Disable consuming-client inference retries: after a disconnect the submitted generation may have run.

## Codex CLI

Check the installed Codex version. For 0.160.0+, use `~/.codex/exetrouter.config.toml` with top-level `model_provider="exetrouter"`, `[features] api_key_model_discovery=true`, and `[model_providers.exetrouter]`: `name="OpenAI"`, the custom `base_url`, `model_catalog_url` pointing to the same router's `/v1/models/codex`, `wire_api="responses"`, `env_key="EXETROUTER_TOKEN"`, `supports_websockets=true`, `request_max_retries=0`, `stream_max_retries=0`. Launch `codex --profile exetrouter`. Discovery is opt-in and under development in 0.160.0; it retrieves metadata without inference. No fixed model or manual export is required. An inherited model preference must be available in the pool; `/model` selects another model. Provider **name `OpenAI`** enables native remote compaction V2.

Migrate legacy `[profiles.exetrouter]`/top-level `profile="exetrouter"` from the main config into the separate profile file, preserving unrelated settings. Remove an ExetRouter `model_catalog_json` override when using live discovery. Older clients without `model_catalog_url` can optionally use `exr models --json --format codex-json` exported to a user-selected absolute path and top-level `model_catalog_json` instead; regenerate snapshots after pool changes. Do not copy the user's Codex OAuth/auth.json into router state or overwrite unrelated config. Confirm the installed client's profile format rather than creating legacy profile tables.

## OpenCode V2

Export `exr models --json --format opencode-jsonc`; merge `providers.exetrouter.models` into the chosen configuration, preserving unrelated fields. Provider: `canonical="openai"`, package `@opencode/ai/providers/openai/responses`, env `EXETROUTER_TOKEN`, settings `baseURL`, `transport="websocket"`, `store=false`, `compaction={type:"native"}`. Select `exetrouter/MODEL_ID`.

Install the repository's `clients/opencode/exetrouter` plugin using an absolute path in `plugins`. It removes unsupported automatic output caps and disables this provider's session retry hook. It does not guarantee every possible client recovery path. Change transport to `http` for an isolated HTTP/SSE test.

## OpenCode V1

The exact checked client is `opencode-ai` 1.18.34, HTTP/SSE only. Use singular `plugin` and `provider`, `npm="@ai-sdk/openai"`, env `EXETROUTER_TOKEN`, options `baseURL`, and provider ID `exetrouter`. Load the separate absolute `file://.../clients/opencode-v1/exetrouter.mjs` plugin. Its public config hook wraps only this provider's transport and turns failures into fixed non-retryable client errors; it also removes caps/sampling and sets store=false. Detailed upstream errors are suppressed. The unmodified V1 client automatically retries errors and is unsuitable for the no-replay contract.

Convert the exported model metadata to V1 descriptors as shown in `docs/compatibility.md`; do not copy V2 configuration. Tools/process resume and four synthetic failure cases passed. V1 real upstream, WebSocket, compaction and long context remain unverified. Avoid experimental native LLM/WebSocket paths and do not apply this bridge to an unreviewed version.

## SDKs and service APIs

Python: `OpenAI(base_url=URL, api_key=os.environ["EXETROUTER_TOKEN"], max_retries=0)`. JavaScript: `new OpenAI({baseURL: URL, apiKey: process.env.EXETROUTER_TOKEN, maxRetries: 0})`.

Supported surfaces: models, Responses JSON/SSE/WS/compact, limited Chat Completions with function tools. Responses forwards `input_image` content to image-capable upstream models; Chat translates ordered user text/`image_url` parts to Responses input. Chat image forwarding has offline fixture coverage, not live vision verification. Use full history, `store=false`; HTTP previous_response_id and output caps are unsupported. Opaque context/open WS stay on their original upstream account. Embeddings/image-generation/audio/files/batches endpoints and upstream API-key support are absent. Check repository `docs/openai-api.md` for exact fields before integrating a new call.

## Local operator only

`exrd admin` works on the server state, not through `exr`. Inspect `exrd --help`/subcommand help before use. Operators create users, register/revoke public SSH keys, export authorized_keys, add/reauthorize/disable OAuth accounts and create/verify/restore backups. Global state options precede subcommands. OAuth login requires operator terminal/device authorization; never request credentials in chat.

Backup destinations and restore state directories must be new. A backup includes both secret keys and encrypted OAuth credentials; it is not additionally encrypted. Offline verification does not establish upstream credential validity. Do not run two services from cloned OAuth state. Operator changes and host deployment need the user's actual requested scope; availability of this skill does not grant it.

Standalone uses the local API address shown in Settings (default http://127.0.0.1:8787/v1). Keep exr or exr serve running. Remote server administration is documented by the separate exrd skill.
