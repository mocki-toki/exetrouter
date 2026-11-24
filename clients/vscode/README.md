# ExetRouter for VS Code

Use your router’s account-visible models in VS Code Chat. The ExetRouter Activity Bar shows **Overview**, **Usage**, **Tokens**, **Models**, and **Settings**. Click the status bar for every account in the local `exr` profile. The extension runs on the desktop UI host, including in a remote workspace.

## Build a VSIX

With Node.js 22.12+ and npm, from `clients/vscode`:

```sh
npm ci
npm run build
npm test
npm run package
```

`npm run package` uses a pinned VSIX packager and writes `exetrouter-0.1.0.vsix` locally. In VS Code, run **Extensions: Install from VSIX…** and choose that file. Requires VS Code 1.146+ with Chat enabled. This is a source-built extension, not a Marketplace listing.

## Connect

1. Configure your normal `exr` profile. If the editor cannot find `exr`, set **ExetRouter: Executable** in user settings to its absolute path.
2. Open **ExetRouter: Connect API Token** and enter an existing router bearer. The prompt shows the destination. To use a different HTTP endpoint, first set **ExetRouter: API Base Url** to the full base including `/v1`. This also enables chat without a compatible native bridge.
3. Select a discovered ExetRouter model in the Chat model picker (enable the provider/models in **Manage Models** if needed). Set **ExetRouter: Reasoning Effort** for models that advertise the chosen level.

The token stays in VS Code SecretStorage, keyed to the normalized endpoint, including port and path. **ExetRouter: Forget API Token** removes it for that endpoint, without revoking the router token. The native dashboard always describes the local `exr` profile; Settings shows the profile and chat endpoints separately.

Use **ExetRouter: Refresh Dashboard and Models** after catalog changes, and **Search Models and Copy ID** to copy a technical model ID. Usage supports 24h, 7d, 30d and 90d, grouped by total, model, user or token. **Show Account Quota History** opens a searchable list of recorded observations, newest first. Failed reads keep the last report for that profile and selection marked **Stale**.

In **Tokens**, select a token or choose **Manage Profile Tokens**. Create, rename, pause, resume, rotate and revoke use the native `exr desktop` bridge and require confirmation for the displayed profile. Create and rotate copy the secret through `exr`; they do not connect it to Chat. If the copy fails, choose **Retry clipboard copy** for the same secret, or discard it. If a mutation’s outcome is unknown, refresh the token list before deciding what to do. No dashboard action sends inference.
