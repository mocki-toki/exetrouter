# Prompt: install the client

Replace the three connection fields, then give this prompt to your coding agent:

```text
Install the ExetRouter exr client from https://github.com/mocki-toki/exetrouter on this computer.
SSH host: api.example.com
SSH port: 2222
SSH username: routercli
Private key path: ~/.ssh/exetrouter_ed25519

Read README.md, docs/installation.md, docs/cli.md and skills/exr/SKILL.md from the repository. Detect the OS/architecture and reuse existing Rust/tooling and connection settings. Install only exr, using an official checksummed release if available or cargo install --locked --git https://github.com/mocki-toki/exetrouter --bin exr. Put it on PATH without a shell alias. Preserve unrelated shell/client settings.

Configure the supplied connection once. Use only an existing protected private key; if no key/registration/verified server fingerprint is available, ask for that missing information. Never disable SSH host-key verification or send a private key anywhere. Ask the operator to register the public key if needed.

Run exr --version, exr doctor --json, exr models --json and exr limits --json. Doctor is a local snapshot; a stale catalog is refreshed by model discovery. Do not generate inference, issue/revoke/rotate tokens or consume reset credits just to test installation. Report what worked and any connection blocker. Install the bundled exr agent skill into the appropriate skills directory if this agent supports local skills. Explain how to open exr and create a token in my own interactive terminal, where its secret goes directly to the clipboard.
```

For standalone installation, use this instead:

```text
Install exr from https://github.com/mocki-toki/exetrouter on this machine for standalone use. Read README.md, docs/installation.md and skills/exr/SKILL.md. Prefer a checksummed matching release, or build locked source. Reuse any existing config/state rather than overwriting it. Configure standalone with a private local state directory and an available loopback port; install only the exr skill. Verify exr --help and read-only account listing without inference. Tell me how to open exr, use Settings to complete my own browser OAuth login, create/copy a bearer in Tokens and keep the API running using the dashboard or exr serve. Do not capture or print real credentials, change SSH, or consume credits.
```
