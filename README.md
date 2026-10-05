<h1 align="center">ExetRouter</h1>

<p align="center">
  Your ChatGPT subscriptions, one shared OpenAI-compatible endpoint.
</p>

<p align="center">
  <img src="docs/assets/tui-overview.jpg"
       alt="ExetRouter terminal dashboard"
       width="680">
</p>

Use your **ChatGPT Plus or Pro subscriptions through one OpenAI-compatible endpoint** for friends, BYOK tools and scripts. Sign in through OAuth, give each person or service a router token, and track usage in the terminal dashboard. Run it locally or on your own server.

Codex CLI can use ChatGPT directly. ExetRouter adds shared access, separate tokens and an account pool. Supported requests consume subscription limits; clients do not receive your ChatGPT credentials.

## How it works

- **`exr`** opens the terminal dashboard: accounts, tokens, usage, subscription limits and settings. Choose **Standalone** and add an OAuth account for personal use — no server or SSH setup required.
- **`exr serve`** keeps the standalone API running without a dashboard.
- **`exrd`** (optional) runs a shared server, with Docker or native deployment. Friends connect with `exr` over restricted SSH. See [Remote Server setup](docs/remote-server.md) for installation guides and an agent prompt.

## Install

Install `exr` to run locally or connect to a shared server.

### Linux

Download and run the binary installer. It selects your architecture and verifies the release checksum; Rust is not required:

```sh
curl --fail --location --proto '=https' --tlsv1.2 \
  https://raw.githubusercontent.com/mocki-toki/exetrouter/main/scripts/install.sh -o /tmp/exetrouter-install.sh
sh /tmp/exetrouter-install.sh --role client
~/.local/bin/exr
```

The client is installed in `~/.local/bin`. Add that directory to your `PATH` to launch it as `exr`. See [installation details](docs/installation.md#release-binaries-on-linux-or-macos) for version selection, custom paths and older Linux systems.

### macOS

Install with Homebrew:

```sh
brew tap mocki-toki/exetrouter https://github.com/mocki-toki/exetrouter
brew install mocki-toki/exetrouter/exr
exr
```

### Nix (Linux or macOS)

With flakes enabled, build and run directly from source:

```sh
nix run github:mocki-toki/exetrouter -- --help
nix profile add github:mocki-toki/exetrouter#exetrouter
exr
```

The package includes both `exr` and `exrd`. See [Nix installation](docs/installation.md#nix-linux-or-macos) for pinned revisions, NixOS/Home Manager configuration, builds and upgrades.

### Build from source

Use Rust 1.88 or newer and a C compiler:

```sh
cargo install --locked --git https://github.com/mocki-toki/exetrouter --bin exr
exr
```

## Usage

### Set up ExetRouter

Run `exr`. On the first launch, the wizard asks how you want to use it:

- **Standalone:** manage your own accounts and run the API on this computer. Open **Settings**, add an account and complete the OAuth sign-in in your browser. The default API endpoint is `http://127.0.0.1:8787/v1`; Settings shows the actual address. Keep the dashboard open, or use `exr serve` for a headless API.
- **Remote Server:** connect to an existing shared server. Enter the SSH host, port, username, private-key path and HTTP API URL in the wizard. Ask the operator to register your public key, provide a verified SSH host key and give you the HTTPS API endpoint. SSH is for the dashboard; Codex, OpenCode and SDKs use the API endpoint.

You can change mode or connection later in **Settings**.

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

Use Codex CLI 0.160.0 or newer. Follow the [Codex profile setup](docs/compatibility.md#codex-cli), then launch `codex --profile exetrouter`. The profile discovers models from your router; `/model` lets you choose one. Discovery is opt-in and marked under development in 0.160.0.

### OpenCode

Check `opencode --version` and export the matching configuration. The export uses the API URL saved during setup.

For V1 (`opencode-ai`):

Create the configuration:

```sh
exr models --json --format opencode-v1-json > exetrouter-v1.json
```

Then start OpenCode:

```sh
OPENCODE_CONFIG="$PWD/exetrouter-v1.json" opencode --model exetrouter/gpt-5.6-sol
```

For V2 (`@opencode/cli`):

Create the configuration:

```sh
exr models --json --format opencode-v2-json --model exetrouter/gpt-5.6-sol > exetrouter-v2.json
```

Then start OpenCode:

```sh
OPENCODE_CONFIG="$PWD/exetrouter-v2.json" opencode --standalone
```

Run from the configuration file’s directory, or use its absolute path in `OPENCODE_CONFIG`.

The example model must appear in `exr models`; replace it if needed. Both configurations read the token from `EXETROUTER_TOKEN` at launch and import your pool's models under **ExetRouter**. Standalone exports use the local API address; keep `exr` or `exr serve` running. Regenerate the configuration after account-pool or model changes.

V2 defaults to WebSocket and native compaction; `--standalone` starts a private OpenCode server with the fresh configuration. OpenCode keeps its standard retry policy. See [OpenCode setup](docs/compatibility.md#opencode-setup) for transport options, configuration merging and verification scope. No plugin is required.

### OpenAI SDKs and other BYOK tools

For a BYOK app, choose a custom OpenAI-compatible endpoint, enter your router token as the API key and select the model ID from **Models**. The app must use a [supported endpoint](docs/openai-api.md).

For Python, install `openai` and create a client with your endpoint and router token:

```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8787/v1",
    api_key=os.environ["EXETROUTER_TOKEN"],
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
});
const response = await client.responses.create({
  model: "MODEL_ID",
  input: "Hello!",
  store: false,
});
console.log(response.output_text);
```

Responses, streaming and supported Chat Completions share the endpoint; see the [SDK/API contract](docs/openai-api.md) for parameters and limitations.

## Privacy and scope

- The router never logs or stores prompts, messages, outputs, tool payloads, authorization headers or request/response bodies. It retains usage counters, operational metadata and keyed context digests. See [security and privacy](SECURITY.md).
- Content passes through server memory. A host operator can inspect memory or replace the software; this is not end-to-end encryption against that operator.
- Personal metadata uses a client-held key created automatically by the native client: token names, account labels and dashboard preferences. Edit account labels from Overview. See [encrypted personal data](docs/private-metadata.md) for key backup and recovery.
- Supported calls use ChatGPT Codex OAuth, not upstream Platform API keys. See the [API contract](docs/openai-api.md) for endpoints and limits.
- Conversations normally stay on their originating account. [Quota failover](docs/account-pool.md#failure-behavior) requires complete current context.
- Management uses restricted SSH and an authenticated Unix socket. There is no public HTTP admin API or per-minute/IP quota.
- Backend compatibility does not establish permission to share subscriptions. Operators remain responsible for their provider agreements.

## Documentation and development

[Installation](docs/installation.md) · [Client CLI/TUI](docs/cli.md) · [Server CLI](docs/server-cli.md) · [Client setup and compatibility](docs/compatibility.md) · [API contract](docs/openai-api.md) · [Account pool](docs/account-pool.md) · [Backup](docs/backup.md)

For development, see [contributing](CONTRIBUTING.md), [architecture](docs/architecture.md) and [remaining work](docs/development-plan.md). Ordinary tests are offline; [live tests](docs/live-testing.md) require explicit authorization and spend subscription quota. Bundled agent instructions are separate for [exr](skills/exr/SKILL.md) and [exrd](skills/exrd/SKILL.md).

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
