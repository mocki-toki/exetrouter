# Contributing

Use Rust 1.88 or newer and a C compiler. SQLite is bundled; no database service is required for tests. Run `cargo test --locked`, `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `python3 scripts/check-publication.py` before proposing changes.

Keep user-facing UI and documentation in English. Preserve HTTP/SSE/WS compatibility, request-once semantics, account affinity, unknown usage and SSH/Unix UID boundaries. Describe the problem and final behavior, with meaningful validation. Do not add payload diagnostics, body capture, broad permissions or a public management HTTP API.

Ordinary tests use synthetic data and temporary private directories. Tests marked ignored may contact OpenAI or run native clients; follow [live testing](docs/live-testing.md) and obtain explicit authorization. Do not submit state databases, keys, `.env` files, customer messages, tokens, logs or compatibility downloads. Publication audit failures print only the file and rule, never the detected value.

On GitHub, CI tests native Linux x86_64/ARM64 and macOS ARM64/Intel builds. Release tags must match the Cargo version. Release automation builds from the tagged source and publishes binaries with SHA-256 checksums, never from a developer's working directory.
