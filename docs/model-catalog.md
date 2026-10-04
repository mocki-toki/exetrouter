# Model catalog and cache affinity

Adapters are tested with Codex `0.159.3` and OpenCode V2 `2.0.21` as of 2026-10-01. Model IDs and capabilities come from the account-visible upstream catalog, never from assumed Platform API pricing/limits.

## Metadata

Schema 6 saves an allowlist of Codex ModelInfo fields: context/compaction limits, reasoning levels/default, modalities, shell/tool modes, instruction settings, Responses Lite and other request-relevant capabilities. Arbitrary account fields are excluded. Per-model metadata is bounded to 256 KiB and the upstream catalog to 1 MiB.

| Authenticated route | Projection |
| --- | --- |
| GET /v1/models | OpenAI list; each model's `exetrouter` field contains allowed metadata. |
| GET /v1/models/codex | `models` with Codex ModelInfo descriptors. |
| GET /v1/models/opencode | `providers.openai.models` configuration fragment. |

Duplicate IDs use minimum numeric limits, intersected reasoning/modalities and positive capabilities enabled only when all accounts support them. Missing/false supports_experimental_context disables that shared capability. Incompatible instructions/tool modes or request-shaping fields reject client export with model_metadata_unavailable; the ordinary ID list remains accessible.

OpenCode receives reported context/input headroom, reasoning variants and modalities. Unreported output limits export as `output: 0`, its unknown-limit convention. Its output reserve is a heuristic; the built-in OpenAI adapter omits output caps rejected by this backend. Public API prices are not inferred.

```sh
exr models
exr models --json --format codex-json > /absolute/path/models.json
exr models --json --format opencode-jsonc > /absolute/path/models.jsonc
```

Codex 0.160.0 can fetch `/v1/models/codex` directly through its provider's `model_catalog_url` with `features.api_key_model_discovery=true`; no file export is required. Keep provider name `OpenAI` for native compaction V2. Older clients can use an absolute `model_catalog_json` path as an optional snapshot. OpenCode V2 merges the exported models into its built-in `openai` provider and enables native compaction; no custom plugin is needed. Catalog files are snapshots: regenerate after pool/catalog changes. See [client configuration and verification scope](compatibility.md#codex-cli).

## Cache keys

Responses/compact/Chat accept an optional prompt_cache_key: a nonempty control-free string up to 64 Unicode characters; null means absent. If absent, the first allowed session hint is considered: session_id, session-id, x-session-id, x-session-affinity, up to 256 bytes. Arbitrary client headers are not forwarded upstream.

The key/hint is HMAC-scoped by user and model and sent as 64 hex characters in the upstream body and session headers. Same raw keys from different users/models remain isolated. Token rotation/restart preserves affinity with the same server key; raw client keys are not persisted/logged.

Without a client key/hint the router still generates a stable user/model cache key, while account selection continues by quota/load. Explicit hints use rendezvous hashing as a soft healthy-account preference. Checkpoints and open WS always take precedence.

The first WS request fixes its upstream cache/session key. Continuations may omit it; changing it produces cache_key_changed before inference. A new affinity needs a new connection/full context. Cache affinity does not guarantee hits or subscription quota savings; actual cached counters are measured separately.
