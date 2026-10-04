# Model catalog and cache affinity

Models and capabilities come from the account-visible upstream catalog. Export verification is recorded in [compatibility](compatibility.md#opencode-model-import); reviewed revisions are pinned in [protocol-sources.json](protocol-sources.json).

## Metadata

Schema 6 saves an allowlist of Codex ModelInfo fields: context/compaction limits, reasoning levels/default, modalities, shell/tool modes, instruction settings, Responses Lite and other request-relevant capabilities. Arbitrary account fields are excluded. Per-model metadata is bounded to 256 KiB and the upstream catalog to 1 MiB.

| Authenticated route | Projection |
| --- | --- |
| GET /v1/models | OpenAI list; each model's `exetrouter` field contains allowed metadata. |
| GET /v1/models/codex | `models` with Codex ModelInfo descriptors. |
| GET /v1/models/opencode-v1 | V1 `provider.exetrouter.models` configuration fragment. |
| GET /v1/models/opencode-v2 | V2 `providers.exetrouter.models` configuration fragment. |

Duplicate IDs use minimum numeric limits, intersected reasoning/modalities and positive capabilities enabled only when all accounts support them. Missing/false supports_experimental_context disables that shared capability. Incompatible instructions/tool modes or request-shaping fields reject client export with model_metadata_unavailable; the ordinary ID list remains accessible.

OpenCode receives reported context/input headroom, reasoning variants and modalities. Unreported output limits export as `output: 0`, its unknown-limit convention. Its output reserve is a heuristic; backend support for requested output caps is validated upstream. Public API prices are not inferred.

```sh
exr models
exr models --json --format codex-json > /absolute/path/models.json
exr models --json --format opencode-v1-json > /absolute/path/exetrouter-v1.json
exr models --json --format opencode-v2-json > /absolute/path/exetrouter-v2.json
```

Codex 0.160.0 supports live catalog discovery; older clients can use an exported snapshot. OpenCode imports the generated configuration with `OPENCODE_CONFIG`, using the standalone API address or saved remote URL (`--base-url` overrides one export). Regenerate snapshots after pool/catalog changes. See [client setup](compatibility.md#configure-your-client) and [output examples](model-export-examples.md).

## Cache keys

Responses/compact/Chat accept an optional prompt_cache_key: a nonempty control-free string up to 64 Unicode characters; null means absent. If absent, the first allowed session hint is considered: session_id, session-id, x-session-id, x-session-affinity, up to 256 bytes. Arbitrary client headers are not forwarded upstream.

The key/hint is HMAC-scoped by user and model and sent as 64 hex characters in the upstream body and session headers. Same raw keys from different users/models remain isolated. Token rotation/restart preserves affinity with the same server key; raw client keys are not persisted/logged.

Without a client key/hint the router still generates a stable user/model cache key, while account selection continues by quota/load. Explicit hints use rendezvous hashing as a soft healthy-account preference. Checkpoints and open WS always take precedence.

The first WS request fixes its upstream cache/session key. Continuations may omit it; changing it produces cache_key_changed before inference. A new affinity needs a new connection/full context. Cache affinity does not guarantee hits or subscription quota savings; actual cached counters are measured separately.
