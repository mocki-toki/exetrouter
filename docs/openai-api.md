# OpenAI-compatible API

ExetRouter supports the routes below, authenticated with a router bearer. It uses ChatGPT Codex OAuth upstream. SDK/native-client verification is recorded in [compatibility](compatibility.md#verified-versions-and-scope); support here does not imply the complete OpenAI Platform API.

| Route | Supported surface |
| --- | --- |
| `GET /v1/models` | Account-visible model list |
| `GET /v1/models/codex`, `/v1/models/opencode-v1`, `/v1/models/opencode-v2` | [Client metadata exports](model-catalog.md) |
| `POST /v1/responses` | JSON or SSE |
| `GET /v1/responses` with WS upgrade | Sequential native `response.create` |
| `POST /v1/responses/compact` | One opaque compaction checkpoint |
| `POST /v1/chat/completions` | Limited Chat JSON or SSE |

Embeddings, image-generation/audio/Realtime, files/uploads/batches and saved-completion CRUD are not implemented. Forwarding a backend option does not add a resource endpoint or a new transport.

## Connect

```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8787/v1",
    api_key=os.environ["EXETROUTER_TOKEN"],
)
model = client.models.list().data[0].id
response = client.responses.create(model=model, input="Hello", store=False)
print(response.output_text)
completion = client.chat.completions.create(
    model=model, messages=[{"role": "user", "content": "Hello"}]
)
print(completion.choices[0].message.content)
```

JavaScript uses `new OpenAI({baseURL, apiKey})`. For remote access, use the operator's HTTPS endpoint. The API-key field holds your router bearer; Platform keys are not upstream credentials.

## Chat request fields

| Field | Supported contract |
| --- | --- |
| model | Required catalog ID, forwarded without substitution. |
| messages | Nonempty complete ordered history: system/developer/user/assistant/tool. |
| content | String or nonempty content-part array. User arrays accept text and image_url parts; other roles accept text only. Assistant null allowed with function calls/refusal. |
| assistant.tool_calls / tool.tool_call_id | Function calls with unique IDs/string arguments; results must reference preceding open calls and be supplied before subsequent generation. |
| assistant.refusal | String mapped to Responses refusal. |
| tools | Up to 128 unique function tools with name/description/parameters/strict. Names: up to 64 ASCII alphanumeric/underscore/hyphen; parameters object; strict defaults false. |
| tool_choice | auto/none/required or named function object; references must exist. |
| parallel_tool_calls | Boolean. |
| stream | Boolean, default false for JSON; true for Chat SSE. |
| stream_options | Streaming only: include_usage boolean; include_obfuscation only false. |
| n / store | n=1 is omitted during conversion; other n values and explicit store values pass to the backend. store defaults false. |
| max_tokens / max_completion_tokens | Translated to max_output_tokens; max_completion_tokens takes precedence when both are present. |
| reasoning_effort | String translated to reasoning.effort; the backend validates available levels. |
| prompt_cache_key | Optional scoped stable key; see catalog contract. |
| response_format | text/json_object/json_schema; schema name/object and optional description/strict (default false), mapped to text.format. |

The adapter validates structures it translates: messages, content, function tools and response formats. Other top-level parameters pass unchanged, including unknown names and null values. The backend validates model options/capabilities; forwarding does not guarantee support. Chat audio and built-in/custom/namespaced tools have no adapter mapping.

### Images

Chat accepts images only in user messages:

```json
{"type":"image_url","image_url":{"url":"https://example.com/image.png","detail":"auto"}}
```

URLs and inline data URLs become ordered Responses `input_image` parts. Optional detail is `auto`, `low`, `high` or `original`; omission preserves the upstream default, while explicit null, unknown fields and images in other roles are rejected before submission.

Responses preserves content arrays, including `input_image` with `image_url`. The router never fetches, decodes, converts or persists image bytes. Upstream validates formats and model support. File uploads/file-ID translation are absent. [Offline fixtures](compatibility.md#image-forwarding-fixtures) verify forwarding and privacy, not live image understanding or every public image format.

## Chat output

JSON returns one choice with assistant text/refusal/function calls and nullable usage. Finish reasons are `stop`, `tool_calls`, `length` for an output-limit terminal, and `content_filter` for that incomplete reason. Unrepresentable output/status fails explicitly; hidden reasoning is not rendered as user text.

SSE emits role/text/refusal/function deltas with stable call IDs/indices. Success ends with a finish chunk and `[DONE]`. `include_usage` adds a final empty-choices usage chunk; earlier chunks have null usage. Interruption/conversion failure emits `upstream_interrupted` without a successful finish or `[DONE]`; sent deltas cannot be recalled.

Prompt/input and completion/output correspond. Cached/reasoning counters are included subsets. Unknown counters remain null, confirmed zeros remain zero; consumers must handle nullable usage. Projection is bounded to 1 MiB and 1024 output items/content parts. Terminal text/arguments are checked against emitted deltas by length/hash; output remains transient.

## Responses input and compaction

Supply full input as a string or array. Strings become user messages, `system` becomes `developer`, absent instructions becomes empty, and `store` defaults false without replacing an explicit value. Other options pass to the backend, including `max_output_tokens`, `context_management`, `background`, `conversation`, `stream_id` and future fields; no static capability denylist is applied.

HTTP JSON is assembled from upstream SSE/completed items; SSE preserves event fields while encoding authenticated opaque values. WS omits the HTTP `stream` field and preserves native event forwarding. Routing ownership, resource bounds and adapter checks still apply.

Native WS services upstream ping/pong and control events between requests. If the upstream closes while no response is in flight, the downstream WS stays open. A separate next request may open a replacement WS on the same eligible account with the same scoped session/thread identity. Incremental input is expanded from the bounded latest completed context; an older or unavailable window returns `context_recovery_unavailable` before inference. Old upstream response IDs are not forwarded to a replacement socket.

Idle metadata is retained as at most one 64 KiB transient notification and delivered after the next `response.created`, preserving native event ordering. Unexpected idle data/terminal/error frames close the downstream connection instead of attributing them to a later request. Upstream controls do not extend the 300-second downstream idle deadline.

When the latest completed context is recoverable, an upstream WS is proactively retired after 30 seconds between requests or five minutes of connection age. An active response is never retired by these limits. The downstream connection remains open; its next request recovers complete context on the same eligible account, without forwarding old socket-specific response IDs or turn state. If no recoverable snapshot exists, proactive retirement is skipped.

Responses Lite preserves its ordered `additional_tools` prefix and custom/namespace definitions. An explicit Lite header needs a recognized native prefix or frame-mode metadata; arbitrary caller headers are not forwarded.

Compact appends `compaction_trigger` to one Responses SSE request and projects `response.compaction` with exactly one opaque checkpoint. Independent requests may select another eligible account; normal checkpoint/socket affinity takes precedence over cache preference.

### Context ownership

- `previous_response_id` is supported only for responses owned by the current WS. HTTP/new-socket previous IDs are unsupported. Bearer validity is checked on every `response.create`.
- `encrypted_content` and native `encrypted_function_args` carry authenticated opaque envelopes; empty argument arrays are valid. The upstream receives the original values after verification. Proofs bind values to their user and issuing account for 24 hours without per-output database rows. Raw values without a router proof are rejected even when their digests remain stored. Unknown/foreign/tampered/expired input returns `context_not_found`; conflicting untransferred owners return `context_account_mismatch`.
- A context snapshot accepts up to 16,384 unique opaque values, including encrypted tool arguments, within the 16 MiB request limit. Duplicate values count once per field kind. Larger snapshots must be compacted.
- Saved conversation IDs must have been issued to the same router user. Authenticated envelopes pin them to the owning account for 24 hours. Unknown/foreign/expired IDs fail before inference. Saved conversations cannot migrate because backend-owned history is unavailable to the router.
- Quota or configured soft-threshold transfer requires complete current context and preserves authorization across concurrent forks. See [account-pool recovery](account-pool.md#failure-behavior) for bounds.

## Request sizes and compression

Inference HTTP bodies and incoming WS messages/frames are bounded to **16 MiB**, independently of output/control limits and model token windows. Oversized HTTP returns 413 `request_too_large` before inference/accounting; oversized WS closes before submission. Malformed JSON returns a redacted `invalid_request_error`. Ingress must accept the same size; bundled nginx templates do.

HTTP accepts identity JSON and `Content-Encoding: zstd`, decoded after authentication. Compressed and decoded bodies each have a 16 MiB limit. Decoding has four global slots without queuing, an 8 MiB history window, a ten-second body-read deadline and a two-second cooperative budget checked per 32 KiB output chunk. Slots remain held until blocking work ends, even after cancellation.

Unsupported/multiple codings return 415 `unsupported_content_encoding`; corrupt/truncated frames return a redacted 400; excessive expansion returns 413 before inference/accounting. This support does not enable compression when Codex's own authentication gates disable it.

## Errors and stream interruption

| Condition | Result |
| --- | --- |
| Chat adapter validation | 400 `invalid_parameter`, with `error.param` |
| Unknown model / no available account | 404 / 503 |
| User concurrency / global saturation | 429 `user_concurrency_limit` / 503 `server_busy`, before inference/accounting; no invented retry time |
| Upstream quota cooldown | `upstream_cooldown` with retry information |
| Transfer registry full | 503 `context_transfer_storage_full` before replacement inference; ordinary proof issuance remains available |
| Operational pause | `upstream_backoff` with retry information |
| Upstream OAuth authentication failure | Redacted `upstream_authentication_error` (502 or started-stream error); refresh belongs to a separate next operation |

Backend HTTP errors retain their status with fixed redacted HTTP/Chat bodies. Bounded upstream error content is never logged, stored or echoed by these adapters. Non-JSON, oversized or unreadable errors use a fixed fallback at the backend status. Native WS events retain their forwarding contract. Upstream authentication errors are distinct from client bearer rejection.

HTTP SSE and native WS emit content-free `ping` events after 15 seconds without a downstream event; clients should ignore unknown types. Heartbeats do not indicate generation progress or reset upstream deadlines: 900 seconds for inference, 300 for metadata.

Native WS additionally sends control pings every 15 seconds to upstream during active requests and to downstream during active/idle periods. Any received upstream frame, including pong, proves peer liveness. Complete upstream silence before the first non-control event is bounded to 90 seconds; after the response starts, the 900-second receive budget applies. A client that sends no frames/pongs while idle is closed after 90 seconds. These liveness controls do not produce model output or renew the application-idle deadline.

An HTTP interruption emits generic `error`, then terminal `response.failed`. Usage may remain unknown. A lost connection or shorter client deadline can prevent error delivery; no false completion is invented.

## Native routing metadata

HTTP returns bounded authenticated `x-codex-turn-state`, unwrapped before upstream dispatch. WS sends handshake `response.metadata` after the first `response.created`, as required by the reviewed OpenCode driver; upstream selection happens after the downstream upgrade. Event metadata also binds issued state before forwarding.

HTTP accepts issued state in its header or `client_metadata["x-codex-turn-state"]`; native WS accepts frame metadata or the upgrade header, projecting accepted header state into frame metadata. Both transports validate ownership/account before recording a generation. Only state issued to the same user is accepted; its authenticated envelope pins the account for 24 hours without a registry entry. Unknown/foreign/expired state and conflicting untransferred opaque context fail before inference.

Quota or configured soft-threshold transfer drops old account-specific state and returns the replacement connection's metadata. Raw values stay transient. An HTTP header that cannot be bound is omitted while the accepted response is drained/accounted; an unbindable metadata event interrupts forwarding.

Additional metadata is limited to bounded `openai-model`, `x-openai-model`, `x-models-etag`, boolean `x-reasoning-included`, and validated numeric `x-codex-primary/secondary-used-percent`, `window-minutes` and `reset-at`. Quota headers report the selected account's observations for native HTTP `/status`; missing values/credits are not fabricated. Credentials and arbitrary headers are excluded.

Reviewed session/thread identities are scoped consistently in headers, frame metadata and serialized `x-codex-turn-metadata`. Tool inventory and other turn fields are preserved. Malformed serialized metadata fails before submission with a fixed error, without echoing content.
