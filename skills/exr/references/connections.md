# Client connections and operator boundary

Use a model ID discovered with `exr models --json`. Obtain the service base URL from the deployment; `https://api.example.com/v1` is a reserved example, while local tests use loopback. Verify the actual service endpoint and TLS certificate.

The library's API-key field holds an **ExetRouter bearer**, supplied to the consuming process through its secret environment. Upstream uses ChatGPT OAuth, not Platform API-key BYOK. A consuming client may retry after a disconnect; the router itself never replays an accepted or ambiguous generation.

## Codex CLI

Check the installed Codex version. For 0.160.0+, use `~/.codex/exetrouter.config.toml` with top-level `model_provider="exetrouter"`, `[features] api_key_model_discovery=true`, and `[model_providers.exetrouter]`: `name="OpenAI"`, the custom `base_url`, `model_catalog_url` pointing to the same router's `/v1/models/codex`, `wire_api="responses"`, `env_key="EXETROUTER_TOKEN"`, `supports_websockets=true`, `request_max_retries=0`, `stream_max_retries=0`. Launch `codex --profile exetrouter`. Discovery is opt-in and under development in 0.160.0; it retrieves metadata without inference. No fixed model or manual export is required. An inherited model preference must be available in the pool; `/model` selects another model. Provider **name `OpenAI`** enables native remote compaction V2.

Migrate legacy `[profiles.exetrouter]`/top-level `profile="exetrouter"` from the main config into the separate profile file, preserving unrelated settings. Remove an ExetRouter `model_catalog_json` override when using live discovery. Older clients without `model_catalog_url` can optionally use `exr models --json --format codex-json` exported to a user-selected absolute path and top-level `model_catalog_json` instead; regenerate snapshots after pool changes. Do not copy the user's Codex OAuth/auth.json into router state or overwrite unrelated config. Confirm the installed client's profile format rather than creating legacy profile tables.

## OpenCode V1 and V2

Resolve the installed version with `opencode --version`. Set the router token in `EXETROUTER_TOKEN`; never put it in the exported file. Export the account-visible catalog using the matching native format. Standalone uses its API address; in remote mode first save the operator’s HTTP API URL with `exr configure --api-url https://api.example.com/v1`. `--base-url` only overrides an individual export:

```sh
exr models --json --format opencode-v1-json > exetrouter-v1.json &&
  OPENCODE_CONFIG="$PWD/exetrouter-v1.json" opencode --model exetrouter/gpt-5.6-sol
```

For V2 use `opencode-v2-json --model exetrouter/gpt-5.6-sol` and `exetrouter-v2.json`, then launch `OPENCODE_CONFIG="$PWD/exetrouter-v2.json" opencode --standalone` so a private OpenCode server reads the fresh configuration. Both exports create a dedicated `exetrouter` provider named `ExetRouter` with models and metadata from our extractor, without inheriting OpenCode's bundled OpenAI catalog. V1 uses `provider`, `npm="@ai-sdk/openai"`, options and a variant object. V2 uses `providers`, package `@opencode/ai/providers/openai/responses`, settings and a variant array; defaults are WebSocket, `store=false` and native compaction. For HTTP/SSE change V2 `providers.exetrouter.settings.transport` to `http`.

The example model must be available in `exr models`. `exetrouter/gpt-5.6-sol` is provider/model notation. Without `--model`, OpenCode follows configured/recent/default selection and may pick another provider. Choose **ExetRouter** in `/models`. Generate the file before each launch to refresh the catalog, and keep this provider's definition out of other configuration files so old model entries do not merge into it. Other settings/providers stay in the regular configuration. No conversion script or plugin is required. OpenCode retains its standard retry policy. See `docs/compatibility.md#opencode-model-import` for verification scope.

V1's experimental WebSocket transport, live upstream, compaction and long context remain unverified with ExetRouter.

## SDKs and service APIs

Python: `OpenAI(base_url=URL, api_key=os.environ["EXETROUTER_TOKEN"], max_retries=0)`. JavaScript: `new OpenAI({baseURL: URL, apiKey: process.env.EXETROUTER_TOKEN, maxRetries: 0})`.

Supported surfaces: models, Responses JSON/SSE/WS/compact, limited Chat Completions with function tools. Responses forwards `input_image` content to image-capable upstream models; Chat translates ordered user text/`image_url` parts to Responses input. Chat image forwarding has offline fixture coverage, not live vision verification. Use full history, `store=false`; HTTP previous_response_id is unsupported; the backend validates requested output caps and other model options. Opaque context/open WS stay on their original upstream account. Embeddings/image-generation/audio/files/batches endpoints and upstream API-key support are absent. Check repository `docs/openai-api.md` for exact fields before integrating a new call.

## Local operator only

`exrd admin` works on the server state, not through `exr`. Inspect `exrd --help`/subcommand help before use. Operators create users, register/revoke public SSH keys, export authorized_keys, add/reauthorize/disable OAuth accounts and create/verify/restore backups. Global state options precede subcommands. OAuth login requires operator terminal/device authorization; never request credentials in chat.

Backup destinations and restore state directories must be new. A backup includes both secret keys and encrypted OAuth credentials; it is not additionally encrypted. Offline verification does not establish upstream credential validity. Do not run two services from cloned OAuth state. Operator changes and host deployment need the user's actual requested scope; availability of this skill does not grant it.

Standalone uses the local API address shown in Settings (default http://127.0.0.1:8787/v1). Keep exr or exr serve running. Remote server administration is documented by the separate exrd skill.
