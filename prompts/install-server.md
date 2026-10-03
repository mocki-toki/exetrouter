# Prompt: install a server

Replace the host, domain and user/key fields. Run this prompt in an agent with access to the intended server:

```text
Deploy ExetRouter from https://github.com/mocki-toki/exetrouter on my Debian/Ubuntu server.
Server access: my existing administrative SSH connection
API domain: api.example.com (DNS should point to this server)
Management SSH port: 2222
First router user: owner
First user's Ed25519 public key file: /path/to/owner.pub

Read README.md, docs/installation.md, docs/server-cli.md, deploy/docker/README.md, deploy/README.md, docs/ssh-management.md and SECURITY.md. Inspect the existing service/ingress/SSH/firewall state and preserve working services. Install a tagged checksummed release when available or build locked source. Prefer the Docker Compose deployment with a versioned official GHCR image and install the exrd skill. Use Compose exec for administration, keep the native restricted gateway separate, and make the host launcher optional. Docker bootstrap uses the container directly; do not start a temporary native API server. Follow its host OpenSSH boundary and credentials/state preparation guide: separate unprivileged service and gateway UIDs, a private database, independent keys, Unix peer validation and a root-controlled per-key forced command. Bind the API to loopback and expose only /v1/ through reviewed TLS ingress. Keep administrative SSH intact. Preserve a concrete rollback before changing an existing service, SSH listener or ingress. Explain any required new exposure before applying it.

Initialize new private state without overwriting existing state, register the supplied public key for the router user and export restricted authorized_keys. Start OAuth device login and give me its URL/code so I can sign in myself; never ask me to paste OAuth tokens. Reuse configured state instead of creating parallel refreshers. Do not print credentials or collect API payloads. Use the privacy-preserving nginx template and disable body/debug capture in any existing proxy for this API.

Validate systemd/sshd/nginx configuration, service health, loopback binding, actual restricted SSH UID isolation, denied key/database access and verified client host keys. Install/configure exr on the intended client if accessible; otherwise give me the client installation command and verified server fingerprint. Validate model discovery and live subscription limits without generating inference or consuming reset credits. Token issuance must happen in my interactive terminal and copy the secret to my clipboard. Do not create temporary public test endpoints or leave test credentials/processes behind. Report installed version, API URL, management connection, completed checks and any blocker.
```
