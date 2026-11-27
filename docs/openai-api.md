# OpenAI-compatible API

ExetRouter supports the routes below, authenticated with a router bearer. The operator selects [Codex, Platform or both](platform-api.md#select-routing-resources) upstream. SDK/native-client verification is recorded in [compatibility](compatibility.md#verified-versions-and-scope); support here does not imply the complete OpenAI Platform API.

| Route | Supported surface |
| --- | --- |
| `GET /v1/models` | Account-visible model list |
| `GET /v1/models/codex`, `/v1/models/opencode-v1`, `/v1/models/opencode-v2` | [Client metadata exports](model-catalog.md) |
| `POST /v1/responses` | JSON or SSE |
| `GET /v1/responses` with WS upgrade | Sequential native `response.create` |
| `POST /v1/responses/compact` | One opaque compaction checkpoint |
| `POST /v1/responses/replay` | ExetRouter extension: exchange an owned checkpoint for a stateless routing ticket, without inference |
| `POST /v1/chat/completions` | Limited Chat JSON or SSE |
| `POST /v1/embeddings` | Unary JSON, using the configured Platform project only |
| `POST /v1/images/generations`, `/v1/images/edits` | [Native unary Codex images](native-tools.md#image-api), JSON, one `gpt-image-2` output |
| `POST /v1/alpha/search` | [Native standalone search](native-tools.md#standalone-web-search), scoped and account-pinned |

Audio/Realtime, files/uploads/batches and saved-completion CRUD are not implemented. Image routes do not provide the complete Platform Images API or bypass Codex's client-side image-tool authorization gate. Forwarding a backend option does not add a resource endpoint or a new transport.

For embeddings, choose an ID with `source: "platform"` and the `embeddings` capability in `GET /v1/models`. The router lists `text-embedding-3-small`, `text-embedding-3-large` and `text-embedding-ada-002` only when observed for a configured project. These models do not appear in generation-only client exports. `client.embeddings.create(model="text-embedding-3-small", input=["first", "second"])` accepts the usual string or token inputs, `float` or `base64` encoding and upstream-supported dimensions. Split large batches to keep each unary JSON reply within the 1 MiB output bound. Requests use Platform funds and do not consume Codex quota.

For agent tool discovery and invocation, use the separate authenticated [MCP endpoint `/mcp`](mcp.md). It reuses these native operations and returns MCP image content without requiring Codex's built-in image extension.

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
| max_tokens / max_completion_tokens | Translated to max_output_tokens; max_completion_tokens takes precedence when both are present. Forwarded to Platform and ignored on Codex. |
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

Supply full input as a string or array. Strings become user messages, `system` becomes `developer`, absent instructions becomes empty, and `store` defaults false without replacing an explicit value. Other options, including `context_management`, `background`, `conversation`, `stream_id` and future fields, pass to the backend for validation unless explicitly documented otherwise.

**Output limits:** The Platform backend receives `max_output_tokens` unchanged. The Codex subscription backend does not accept it, so the router omits it from outbound Codex Responses requests, including translated Chat, compaction and WebSocket requests. It does not cap generated tokens or subscription quota on Codex. The original limit is kept for any otherwise eligible quota-only fallback to Platform; supplying a limit does not affect routing or enable fallback.

HTTP JSON is assembled from upstream SSE/completed items; SSE preserves event fields while encoding authenticated opaque values. WS omits the HTTP `stream` field and preserves native event forwarding. Routing ownership, resource bounds and adapter checks still apply.

Fast mode is an operator-controlled exception to option forwarding: client `service_tier: "priority"` (also `"fast"`) is removed before upstream submission unless permitted by the global server setting or an explicit override on the authenticated token. The default is off; other service tiers are unaffected. This applies to Responses JSON/SSE/WS, Chat and compaction, including standalone. A policy-storage failure refuses the generation instead of allowing Fast. Model-catalog Fast support may still be advertised to clients; it does not override this server policy.

Native WS services upstream ping/pong and control events between requests. If the upstream closes while no response is in flight, the downstream WS stays open. A separate next request may open a replacement WS on the same eligible account with the same scoped session/thread identity. Incremental input is expanded from the bounded latest completed context; an older or unavailable window returns `context_recovery_unavailable` before inference. Old upstream response IDs are not forwarded to a replacement socket.

Large complete generations may instead use upstream HTTP SSE while the client continues using native WS. HTTP has no socket-local response IDs: incremental input must be expanded from the bounded latest completed context. If expansion is unavailable, the router returns `previous_response_not_found` **before inference and accounting**; the client must supply full current input. Reviewed OpenCode V2 performs one full-context recovery and may reopen its WS. Other clients must implement this recovery or resend full input explicitly. No history is truncated or retained beyond the existing recovery budget. Normal authorization/account affinity still applies on the full request, including after reconnect.

When a native WS error reports `upstream_websocket_unavailable`, its message includes the downstream `client_connection_id` and `frame_sequence`. Look up that pair in the JSON log's `span` on `upstream_websocket_handshake_failed` events. Each failed attempt records elapsed milliseconds, a fixed reason and the numeric HTTP status when available. A Chat handshake carries its HTTP `request_id` in the span. The measurements cover the attempt to open the upstream connection before generation submission.

An interrupted native WS request retains error code/type `upstream_interrupted`, except the request-specific upstream size rejection described below. Its message includes a fixed reason/stage, an optional numeric close code and the router-generated request ID for correlation with the server's `upstream_stream_interrupted` summary. No upstream close-reason text or transport-error payload is exposed. Delivery can fail if the client connection is already lost. A close code of 1000 does not mean an in-flight response completed; completion requires its terminal event. Client retries remain client-controlled; the router does not infer retry linkage or the user's cancellation intent from adjacent requests.

Idle metadata is retained as at most one 64 KiB transient notification and delivered after the next `response.created`, preserving native event ordering. Unexpected idle data/terminal/error frames close the downstream connection instead of attributing them to a later request. Upstream controls do not extend the 300-second downstream idle deadline.

When the latest completed context is recoverable, an upstream WS is proactively retired after 30 seconds between requests or five minutes of connection age. An active response is never retired by these limits. The downstream connection remains open; its next request recovers complete context on the same eligible account, without forwarding old socket-specific response IDs or turn state. If no recoverable snapshot exists, proactive retirement is skipped.

Responses Lite preserves its ordered `additional_tools` prefix and custom/namespace definitions. The explicit `x-openai-internal-codex-responses-lite: true` mode is preserved even without a visible tool prefix, including compaction and prefix-free continuations. Without that header, the router derives Lite mode from the native prefix or recognized frame-mode metadata. Other explicit Lite values are rejected; arbitrary caller headers are not forwarded. Context ownership checks remain unchanged.

Compact appends `compaction_trigger` to one Responses SSE request and projects `response.compaction` with exactly one opaque checkpoint. Independent requests may select another eligible account; normal checkpoint/socket affinity takes precedence over cache preference.

This single-checkpoint translation is Codex-specific. Configured
[Platform routes](platform-api.md) call the public unary compact endpoint and
preserve its complete output array, including a visible prefix. Their opaque
values remain pinned to the issuing project; Codex/Platform state is not assumed
portable. Platform-backed WS supports generating requests through HTTP SSE, but
rejects `generate=false` locally without submission/accounting or closing the WS.

### Context ownership

Ordinary full-history clients do not need a plugin or a replay ticket. A client
recovering an encrypted legacy checkpoint can exchange it with `POST /v1/responses/replay`:
send `{chat, model, checkpoint}`, where `chat` is a lowercase 64-hex chat digest.
A `204` carries `x-exetrouter-replay-ticket` when the linked resource is
eligible. An explicit `x-exetrouter-replay-mode: rebuild` on the exchange also
allows the original resource if it is eligible, for example when replacing a
legacy checkpoint with a textual summary. Use the ticket with
`x-exetrouter-replay-chat` on Responses HTTP requests or the
WebSocket upgrade, and send the rebuilt transcript as the regular input. The
ticket is scoped to the bearer user, chat and wire model. It keeps routing within
the same linked pair without granting access to any opaque input. The ticket
and chat digest are never sent upstream and do not change prompt cache keys.

- `previous_response_id` is supported only for responses owned by the current WS. HTTP/new-socket previous IDs are unsupported. Bearer validity is checked on every `response.create`.
- `encrypted_content` and native `encrypted_function_args` carry authenticated opaque envelopes; empty argument arrays are valid. The upstream receives the original values after verification. Proofs bind values to their user and issuing account without a router-imposed time limit or per-output database rows, including proofs with a passed legacy expiry. Restart preserves them with the existing key and user/account state. Current bearer validity and account authorization/eligibility still apply. Raw values without a router proof are rejected even when their digests remain stored. Unknown/foreign/tampered input returns `context_not_found`; conflicting untransferred owners return `context_account_mismatch`.
- A context snapshot accepts up to 16,384 unique opaque values, including encrypted tool arguments. Duplicate values count once per field kind. Larger snapshots must be compacted; there is no router-defined request byte ceiling.
- Saved conversation IDs must have been issued to the same router user. Authenticated envelopes pin them to the owning account without router-side expiry; the backend may still expire its own saved history. Unknown/foreign/tampered IDs fail before inference. Saved conversations cannot migrate because backend-owned history is unavailable to the router.
- Quota, explicit deactivation or automatic soft transfer requires complete current context and preserves authorization across concurrent forks. After deactivation, the next request selects an eligible active replacement; a bound WS recovers its latest completed window and retains the downstream connection. Saved conversations cannot migrate. Economic urgency does not bypass ownership or availability checks. See [account-pool recovery](account-pool.md#failure-behavior) for bounds.
- A transfer may add empty router-only reasoning items carrying authenticated `exrtr1` receipts. Preserve them alongside encrypted reasoning/tool history, including across process reopen. They prove portability of exact ancestor values without server registry growth. The router removes these items before dispatch; compaction replaces them with its normal checkpoint. New transfers work even when the legacy registry is full. See [receipt bounds and rollback](account-pool.md#opaque-context).

## Request sizes and compression

Inference HTTP bodies and incoming WS messages/frames have **no router-defined byte ceiling**. OpenAI validates model token windows and its own transport limits; a model's token window is not a WebSocket byte limit. Bundled nginx templates disable their body-size ceiling as well. Requests are parsed in memory, so removing this ceiling does not guarantee unlimited host memory. Authorization, concurrency limits, decoding deadlines and independent output/control limits remain in force. Malformed JSON returns a redacted `invalid_request_error`. Retained WS recovery uses at most 16 MiB per window and 64 MiB globally, with large windows compressed in memory and a separate 64 MiB decoded recovery bound. These optional snapshot budgets are not input acceptance limits. Exact large histories can therefore continue as native deltas without asking the client to reconstruct full input on every tool turn.

HTTP accepts identity JSON and `Content-Encoding: zstd`, decoded after authentication. Neither compressed nor decoded bodies have a router-defined byte ceiling. Decoding has four global slots without queuing, an 8 MiB history window, a ten-second body-read deadline and a two-second cooperative budget checked per 32 KiB output chunk. Slots remain held until blocking work ends, even after cancellation. A decoder allocation failure returns 503 `server_busy`, not an invented context-limit error.

Unsupported/multiple codings return 415 `unsupported_content_encoding`; corrupt/truncated frames return a redacted 400. This support does not enable compression when Codex's own authentication gates disable it.

Upstream WS negotiates RFC 7692 `permessage-deflate`, matching the reviewed Codex 0.164.0 transport. When accepted, the complete request passes through the negotiated compressor without changing its input. When declined, plain text messages use continuation frames of at most 1 MiB. Incoming upstream messages remain bounded to 1 MiB **after decompression**, independently of input acceptance. Negotiation does not enable downstream client compression or forward arbitrary extension headers. Compressed outgoing messages require transient buffers proportional to the input; they are not constrained by the old 2 MiB fragment write-buffer ceiling.

Chat Completions uses upstream HTTP/SSE by default, including when the client requests a single JSON response. The optional `X-ExetRouter-Upstream-Transport` header accepts `http_sse` to name the default or `websocket` to prefer WebSocket for an eligible request.

Complete generation requests whose serialized prepared JSON reaches 16 MiB prefer upstream HTTP SSE **before submission**. This is a transport-selection heuristic, not an input ceiling or a claimed OpenAI size contract. HTTP validates its own limits. Existing native connections can switch transports between completed requests on the same eligible account; scoped identities and input are preserved, old socket-specific turn state is discarded, and the client-facing WS remains open. Subsequent generations stay on HTTP for that binding. Native `generate=false` warmup remains WS-only and is never converted into HTTP generation. Chat uses the same pre-submission preference. HTTP headers are projected as bounded `response.metadata` after `response.created`, with authenticated turn state, and actual upstream transport is recorded in usage. Client disconnect/shutdown cancels the HTTP wait without a detached task; an unobserved outcome remains unknown.

If upstream still closes with 1009 (`Message Too Big`), return `upstream_request_too_large` without account-wide operational backoff, automatic replay or account transfer. HTTP Chat using upstream WS returns 413 for JSON or a started-stream error for SSE. Other upstream interruptions retain their existing backoff/retry contract. Neither fragmentation nor compression removes a provider's whole-message or decoded-size limit; source compatibility and synthetic fixtures do not prove that any particular large live context is accepted.

## Errors and stream interruption

| Condition | Result |
| --- | --- |
| Chat adapter validation | 400 `invalid_parameter`, with `error.param` |
| Unknown model / no available account | 404 / 503 |
| User concurrency / global saturation | 429 `user_concurrency_limit` / 503 `server_busy`, before inference/accounting; no invented retry time |
| Upstream quota cooldown | `upstream_cooldown` with retry information |
| Operational pause | `upstream_backoff` with retry information |
| Upstream OAuth authentication failure | Redacted `upstream_authentication_error` (502 or started-stream error); refresh belongs to a separate next operation |

Backend HTTP errors retain their status with fixed redacted HTTP/Chat bodies. Bounded upstream error content is never logged, stored or echoed by these adapters. Non-JSON, oversized or unreadable errors use a fixed fallback at the backend status. Native WS events retain their forwarding contract. Upstream authentication errors are distinct from client bearer rejection.

Operators can correlate inference refusals through `upstream_response_rejected` logs: router request ID, upstream transport, optional upstream request ID/status and exact allowlisted `error_code`, `error_type` and `error_param` labels. Unknown or missing labels are `unknown`, not a guessed cause. Labels such as `unsupported_parameter` and `max_output_tokens` can identify a validation refusal without retaining its message or arbitrary parameter paths. The HTTP and upstream-event paths use the same classifier; redacted client bodies and native event forwarding are unchanged. Logs cannot reconstruct historical errors recorded before this diagnostic, and no diagnostic triggers a retry.

If an upstream HTTP POST fails before response headers arrive, `upstream_http_post_failed` records the router request ID, `stage=before_headers`, elapsed milliseconds and a fixed `transport_error_kind`. During HTTP stream reads, `upstream_stream_interrupted` includes the request duration and a transport category when the HTTP client reports one. SSE decoder failures and clean EOF before completion have separate reasons. For an HTTP quota retry, `upstream_http_quota_retry` links the original request ID to the new attempt ID. The category reflects the available client-library evidence. A failure before headers does not establish whether the provider received the POST.

After a streaming HTTP response starts, `http_sse_finished` records why the worker stopped: a terminal event, a downstream disconnect or queue timeout, shutdown, an upstream read failure, or the local observation/projection stage that failed. Correlate its request ID with `upstream_stream_interrupted` for transport detail. `upstream_completed_seen` reports an observed `response.completed`; for Chat, `chat_done_queued` means `[DONE]` entered the downstream queue, not that the client received it. HTTP 200 alone does not establish either outcome.

HTTP SSE and native WS emit content-free `ping` events after 15 seconds without a downstream event; clients should ignore unknown types. Heartbeats do not indicate generation progress or reset upstream deadlines: 900 seconds for inference, 300 for metadata.

For long Chat completions behind a proxy, request `stream: true` and consume the stream through its finish chunk and `[DONE]`. With JSON, upstream progress produces no downstream bytes until completion. When the HTTP adapter reads an upstream WebSocket, its interruption log distinguishes read errors, invalid events, timeouts and closes; only the fixed error category and optional numeric close code are recorded.

Native WS additionally sends control pings every 15 seconds to upstream during active requests and to downstream during active/idle periods. Any received upstream frame, including pong, proves peer liveness. Complete upstream silence before the first non-control event is bounded to 90 seconds; after the response starts, the 900-second receive budget applies. A client that sends no frames/pongs while idle is closed after 90 seconds. These liveness controls do not produce model output or renew the application-idle deadline.

An HTTP interruption emits generic `error`, then terminal `response.failed`. Usage may remain unknown. A lost connection or shorter client deadline can prevent error delivery; no false completion is invented.

## Native routing metadata

HTTP returns bounded authenticated `x-codex-turn-state`, unwrapped before upstream dispatch. WS sends handshake `response.metadata` after the first `response.created`, as required by the reviewed OpenCode driver; upstream selection happens after the downstream upgrade. Event metadata also binds issued state before forwarding.

HTTP accepts issued state in its header or `client_metadata["x-codex-turn-state"]`; native WS accepts frame metadata or the upgrade header, projecting accepted header state into frame metadata. Both transports validate ownership/account before recording a generation. Only state issued to the same user is accepted; its authenticated envelope pins the account without router-side expiry or a registry entry. Upstream state availability remains backend-owned. Unknown/foreign/tampered state and conflicting untransferred opaque context fail before inference.

Quota, deactivation or automatic soft transfer drops old account-specific state and returns the replacement connection's metadata. A replayed turn token is authenticated and discarded if its account has already been retired on that WS or if valid transfer receipts cover its account's ancestors in the current input. Receipt-based retirement also works on HTTP and new sockets after restart. This does not authorize unverified state or unrelated mixed-account history. Raw values stay transient. An HTTP header that cannot be bound is omitted while the accepted response is drained/accounted; an unbindable metadata event interrupts forwarding.

Additional metadata is limited to bounded `openai-model`, `x-openai-model`, `x-models-etag`, boolean `x-reasoning-included`, and validated numeric `x-codex-primary/secondary-used-percent`, `window-minutes` and `reset-at`. Quota headers report the selected account's observations for native HTTP `/status`; missing values/credits are not fabricated. Credentials and arbitrary headers are excluded.

Clients may opt into content-free account observation with
`x-exetrouter-observe-account: true`. Successful HTTP replies add
`x-exetrouter-account` and `x-exetrouter-resource` (`codex` or `platform`);
native WS adds both in `response.metadata` after each
generation's `response.created`, including reused connections and account
transfers. The resource is the one actually serving the request. Both resources
of one parent account report the same `x-exetrouter-account`, a domain-separated user/account HMAC alias matching that user's
authenticated management `quota_accounts[].observation_key`, not an upstream
identity, account selector, ownership proof or credential. Other users receive
different aliases. The opt-in header is not forwarded upstream; ordinary clients
keep the existing metadata behavior. There is no inference-side management API,
new state table or conversation payload retention.

Reviewed session/thread identities are scoped consistently in headers, frame metadata and serialized `x-codex-turn-metadata`. Tool inventory and other turn fields are preserved. Malformed serialized metadata fails before submission with a fixed error, without echoing content.
