<h1 align="center">ExetRouter</h1>

<p align="center">
  Your ChatGPT subscriptions, one shared OpenAI-compatible endpoint.
</p>

<p align="center">
  <img src="docs/assets/tui-overview.jpg"
       alt="ExetRouter terminal dashboard"
       width="680">
</p>

Turn your **ChatGPT Plus or Pro subscription into a shared OpenAI-compatible endpoint** for friends, BYOK tools and scripts. Give each person or service their own router token, keep your ChatGPT login private, and see usage grouped by user in the terminal dashboard. Run it locally for your own tools or host it on a server you control.

Codex CLI already supports ChatGPT subscriptions directly. ExetRouter adds a common endpoint for multiple people and tools, separate access tokens and shared account-pool management. Sign in to upstream accounts once through OAuth; clients receive router tokens instead of your ChatGPT credentials. Supported requests use the subscriptions' available limits rather than a separately billed OpenAI Platform API key.

## What it's for

**Share access without sharing your ChatGPT login.** Host one account or a pool on your server and issue individual tokens to friends and services. Each token belongs to a router user, so usage can be tracked by user and access revoked independently. Codex CLI and OpenCode connect to the same endpoint; restricted SSH provides dashboard access, while applications use the API over HTTPS.

**Connect BYOK clients and scripts to the same endpoint.** Give a compatible editor, agent or app an ExetRouter bearer token and your router's `/v1` URL. Scripts and services can use OpenAI SDKs for supported Responses, SSE or WebSocket streaming, or supported Chat Completions. Clients must support a custom endpoint and the router's API contract; this is not the entire OpenAI API.

**See who uses the pool and how much quota is left.** The dashboard combines per-user usage, model statistics and subscription limits. Each person can manage their own tokens. Commands also provide structured reports for automation: `exr tokens --json`, `exr usage --json` and `exr limits --json`.

## How it works

- **`exr`** opens the terminal dashboard: accounts, tokens, usage, subscription limits and settings. Choose **Standalone** and add an OAuth account for personal use — no server or SSH setup required.
- **`exr serve`** keeps the standalone API running without a dashboard.
- **`exrd`** runs a shared server, with Docker or native deployment. Friends configure `exr` once to connect over restricted SSH; private SSH keys stay on their devices.

Responses, native compaction, streaming, account affinity, bounded automatic quota failover and limited Chat Completions are implemented and tested. Responses forwards image inputs to capable upstream models, including images sent by Codex; Chat Completions translates user image parts into the same input format. Chat image forwarding is covered by offline fixtures; actual vision behavior depends on the upstream model and remains separately unverified. Embeddings, image generation, audio, Assistants and other unsupported endpoints are not provided. Subscription availability and upstream behavior can change; see [compatibility](docs/compatibility.md).

## Install

Install `exr` for standalone use or connection to an existing server. For source installation, use Rust 1.88 or newer and a C compiler; remote mode also needs OpenSSH:

```sh
cargo install --locked --git https://github.com/mocki-toki/exetrouter --bin exr
exr
```

[Full installation guide](docs/installation.md) · [Docker server](deploy/docker/README.md) · [Native server](deploy/README.md) · [GitHub releases](https://github.com/mocki-toki/exetrouter/releases)

Want an agent to install it? Copy the [client prompt](prompts/install-client.md) or the [server prompt](prompts/install-server.md), fill in the connection/domain fields, and send it to your agent. The instructions include checks and leave OAuth login and secret issuance to the user's interactive terminal.

## Usage

### Set up ExetRouter

Run `exr`. On the first launch, the wizard asks how you want to use it:

- **Standalone:** manage your own accounts and run the API on this computer. Open **Settings**, add an account and complete the OAuth sign-in in your browser. The default API endpoint is `http://127.0.0.1:8787/v1`; Settings shows the actual address. Keep the dashboard open, or use `exr serve` for a headless API.
- **Remote Server:** connect to an existing shared server. Enter the SSH host, port, username and private-key path in the wizard. Ask the operator to register your public key, provide a verified SSH host key and give you the HTTPS API endpoint. SSH is for the dashboard; Codex, OpenCode and SDKs use the API endpoint.

You can change mode or connection later in **Settings**. No shell alias or connection flags are needed.

In **Tokens**, create a token for the tool you want to connect. Its secret is copied directly to your clipboard. You need the **API endpoint and router token** to connect. For tools that ask for a model ID, choose one in **Models** and press Enter to copy it; Codex can select a model interactively.

Store the token in your password manager or service secret store. For the examples below, expose it to the client process as `EXETROUTER_TOKEN`. In Bash or Zsh, this reads a pasted token without displaying it or putting it in shell history:

```sh
printf 'Paste router token: '
read -rs EXETROUTER_TOKEN
printf '\n'
export EXETROUTER_TOKEN
```

Use `http://127.0.0.1:8787/v1` for the default standalone setup. For remote access, replace `https://api.example.com/v1` in the examples with the HTTPS endpoint your operator supplied. Where an example uses `MODEL_ID`, replace it with the model copied from **Models**.

### Codex CLI

Use Codex CLI **0.160.0 or newer**. Create `~/.codex/exetrouter.config.toml` with the following settings. Profiles use separate files; do not put this inside a `[profiles.exetrouter]` table in `config.toml`.

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

Replace both URLs with your router's address. For standalone use, they are `http://127.0.0.1:8787/v1` and `http://127.0.0.1:8787/v1/models/codex`.

Start `codex --profile exetrouter` from the terminal where the token is available. Codex fetches model IDs and metadata from the router; no catalog export or fixed `model` setting is needed. It inherits your existing model preference, or chooses a catalog default if none is configured. Use `/model` to choose another model available in your pool. Keep the provider name `OpenAI` for native compaction. Automatic catalog discovery is an opt-in Codex feature, marked under development in 0.160.0.

**Migrating from the old README:** move ExetRouter settings to `exetrouter.config.toml`, then remove the legacy `[profiles.exetrouter]` table and any top-level `profile = "exetrouter"` from `~/.codex/config.toml`. Remove an old ExetRouter `model_catalog_json` setting to use the live catalog. Preserve unrelated settings and your existing ChatGPT login. See [configuration details and older clients](docs/compatibility.md#codex-cli) and [official Codex profiles](https://developers.openai.com/codex/config-advanced#profiles).

### OpenCode V2

Use the bundled bridge plugin with the tested OpenCode V2 client. If you installed only the binary, download the repository for the plugin:

```sh
git clone https://github.com/mocki-toki/exetrouter.git
exr models --json --format opencode-jsonc > exetrouter-models.json
```

Merge this into your OpenCode `opencode.jsonc`, preserving existing providers and plugins. Replace the plugin path with the absolute path to `clients/opencode/exetrouter` in your checkout:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": ["/absolute/path/exetrouter/clients/opencode/exetrouter"],
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
      "models": {"MODEL_ID": {}}
    }
  }
}
```

Replace the `models` object with `providers.exetrouter.models` from the exported `exetrouter-models.json`; it supplies context limits, capabilities and reasoning variants. Start OpenCode from the terminal where the token is available and select `exetrouter/MODEL_ID`. The bridge removes unsupported output-token caps and disables this provider's session retry hook. These settings target **OpenCode V2**, not V1; see the [compatibility matrix and configuration](docs/compatibility.md#opencode-v2).

### OpenAI SDKs and other BYOK tools

For a BYOK app, choose a custom OpenAI-compatible endpoint, enter your router token as the API key and select the model ID from **Models**. The app must use a [supported endpoint](docs/openai-api.md).

For Python, install `openai` and create a client with your endpoint and router token:

```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8787/v1",
    api_key=os.environ["EXETROUTER_TOKEN"],
    max_retries=0,
)
response = client.responses.create(
    model="MODEL_ID",
    input="Hello!",
    store=False,
)
print(response.output_text)
```

For JavaScript, install `openai` and use the same values:

```javascript
import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "http://127.0.0.1:8787/v1",
  apiKey: process.env.EXETROUTER_TOKEN,
  maxRetries: 0,
});
const response = await client.responses.create({
  model: "MODEL_ID",
  input: "Hello!",
  store: false,
});
console.log(response.output_text);
```

Retries are disabled because a generation may have run even if its response was lost. Responses, streaming and supported Chat Completions share the endpoint; see the [SDK/API contract](docs/openai-api.md) for parameters and limitations.

## Privacy and scope

- The router does **not log or store prompts, messages, outputs, tool arguments/results, authorization headers or request/response bodies**. Operational logs contain fixed events, generated IDs, statuses and timing; usage stores model IDs and numeric counters. Context bindings store keyed digests, not conversation content. See [privacy details](SECURITY.md).
- A proxy necessarily processes content in memory. This is not end-to-end encryption against an operator who controls the host or can replace its software. The project provides no conversation-history or payload-inspection admin feature.
- Upstream uses ChatGPT Codex OAuth. OpenAI Platform API keys are not upstream credentials. BYOK applications can use a router bearer and your deployment's custom endpoint within the supported API contract.
- Opaque context and open WebSockets stay on their originating account. The router never silently migrates an existing conversation or retries a possibly submitted inference request.
- Management uses restricted SSH and an authenticated Unix socket; there is no public HTTP admin endpoint. Resource concurrency protects the process; per-minute/IP quotas are not imposed.
- Compatibility is not an official guarantee of backend stability or permission to share subscriptions. Operators are responsible for their provider agreements.

## Develop

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/check-publication.py
```

Ordinary tests are offline and use synthetic fixtures. Live tests are explicitly opt-in; they spend subscription quota and must not run in CI. The separate [exr skill](skills/exr/SKILL.md) and [exrd skill](skills/exrd/SKILL.md) document client/standalone and server operation. See [contributing](CONTRIBUTING.md) and [the roadmap](docs/development-plan.md).

[Client CLI/TUI](docs/cli.md) · [Server CLI](docs/server-cli.md) · [API contract](docs/openai-api.md) · [Client compatibility](docs/compatibility.md) · [Architecture](docs/architecture.md) · [Model metadata](docs/model-catalog.md) · [Account pool](docs/account-pool.md) · [Backup](docs/backup.md) · [Upstream contract](docs/upstream-contract.md)

## License

[MIT](LICENSE). Rust dependencies retain their own licenses.

## Star History

<a href="https://www.star-history.com/?repos=mocki-toki%2Fexetrouter&type=date&legend=top-left">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=mocki-toki/exetrouter&type=date&theme=dark&legend=top-left" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=mocki-toki/exetrouter&type=date&legend=top-left" />
    <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=mocki-toki/exetrouter&type=date&legend=top-left" />
  </picture>
</a>
