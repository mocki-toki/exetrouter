# Remote Server

Run `exrd` on your server to share a ChatGPT account pool with friends, tools and services. Each user gets separate access and tokens; OAuth accounts stay on the server. The `exr` dashboard connects over restricted SSH, while applications use the HTTPS API.

## Install manually

Choose the guide for your deployment:

- [Docker Compose](../deploy/docker/README.md): versioned official image and a separate native SSH gateway.
- [Native server](../deploy/README.md): systemd service, OpenSSH gateway and nginx ingress.

Both guides target Debian/Ubuntu and cover private state, OAuth, TLS and verification. For an existing server, follow [client installation](installation.md#configure-once). Operator commands are in [server-cli.md](server-cli.md).

## Install with an agent

Choose **Docker Compose** or **native systemd** in the deployment field, fill in the domain/user/public-key details, then give this prompt to an agent with administrative access to the server:

```text
Install ExetRouter from https://github.com/mocki-toki/exetrouter on my Debian/Ubuntu server.
Deployment: Docker Compose (change to native systemd if preferred)
Server access: my existing administrative SSH connection
API domain: api.example.com (DNS points to this server)
Management SSH port: 2222
First router user: owner
First user's Ed25519 public key: /path/to/owner.pub

Read SECURITY.md, docs/server-cli.md and docs/ssh-management.md. Follow deploy/docker/README.md for Docker, or deploy/README.md for native systemd. Install the bundled exrd skill.

Inspect existing services, ports, SSH, ingress and firewall. Preserve working services, administrative SSH, custom settings, state and keys. For an existing installation, create and verify a private backup and retain a rollback version/config before changes; review schema compatibility. Never initialize over existing state.

For Docker, use a versioned official GHCR image, Compose administration and a matching native host gateway; bootstrap directly in the container. For native, install a checksummed release or locked tagged source and the systemd service. Keep separate unprivileged service/gateway UIDs, private state and independent keys. The gateway must have no Docker or database/key access. Publish the API only through reviewed TLS ingress; its host listener stays on loopback. Preserve SSE/WS and disable payload/debug/body capture.

Create the router user, register only the supplied public key and install root-controlled per-key forced commands. Start OAuth device login and give me its URL/code so I can sign in in my browser. Never request pasted OAuth tokens, expose credentials or run parallel copies of refresh state.

Validate systemd/sshd/nginx configuration, health, listeners, UID/socket isolation, denied gateway database/key access and public API bearer rejection. Check Doctor and model discovery without inference. Do not open Overview or run live Limits during automated checks: they may trigger weekly activation. Do not consume reset credits. Give me the verified SSH host fingerprint and client setup instructions; token issuance belongs in my interactive terminal and copies the secret to the clipboard.

Remove temporary test processes/files. Report the installed version, API URL, management connection, completed checks and any blocker.
```
