# OpenAI SDK and model API contract

ExetRouter supports authenticated models, Responses JSON/SSE/WS/compact and limited Chat Completions. Python openai 3.22.1 and JavaScript openai 7.25.0 passed mock integration tests. Real protocol tests use the Rust harness; this does not verify every parameter/model through the SDK.

## Connect

```python
import os
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8787/v1",
                api_key=os.environ["EXETROUTER_TOKEN"], max_retries=0)
model = client.models.list().data[0].id
response = client.responses.create(model=model, input="Hello", store=False)
print(response.output_text)
completion = client.chat.completions.create(
    model=model, messages=[{"role": "user", "content": "Hello"}])
print(completion.choices[0].message.content)
```

JavaScript uses `new OpenAI({baseURL, apiKey, maxRetries: 0})`. The API-key field holds a router bearer; upstream uses operator ChatGPT OAuth. BYOK applications need a custom endpoint and supported calls. Platform upstream keys are unsupported. Use the endpoint supplied by your operator; https://api.example.com/v1 is a reserved example.

Disable client retries as shown. A submitted generation may have run even when its response was lost. The router does not repeat it.

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
| n / store | Only n=1 and store=false; optional. |
| reasoning_effort | none/minimal/low/medium/high/xhigh/max, translated to reasoning.effort. |
| prompt_cache_key | Optional scoped stable key; see catalog contract. |
| response_format | text/json_object/json_schema; schema name/object and optional description/strict (default false), mapped to text.format. |

Actual model support is required for reasoning/schema/tool capabilities. Unknown parameters are rejected even when null. Supported optional top-level null fields mean absent. Temperature/top_p/output caps/stop/seed/logprobs/penalties/user/metadata/legacy functions/audio are unsupported. Chat audio/built-in/custom/namespaced tools are unsupported.

Validation returns 400 invalid_parameter with error.param, unknown model 404, unavailable account 503. Local user concurrency returns 429 user_concurrency_limit, global saturation 503 server_busy, before inference/usage. No fabricated retry time is given for resource slots.

Chat user images use `{"type":"image_url","image_url":{"url":"https://example.com/image.png","detail":"auto"}}` or an inline image data URL. Detail may be `auto`, `low`, `high` or `original`; actual support depends on the upstream model. Omission preserves the upstream default; an explicit null image detail is rejected. Image parts in other roles and unknown image fields are rejected before submission. The router forwards references without fetching or decoding their bytes; upstream validates image format and content. No Files upload or file-ID adapter is provided. Offline unit and integration tests verify multiple-image order and detail, JSON/SSE output over HTTP/WS upstream transports, rejection before submission and usage, and absence of image references from logs and state. Synthetic forwarding fixtures do not prove live image understanding. See the [official vision guide](https://developers.openai.com/api/docs/guides/images-vision).

## Chat output

JSON returns one choice, assistant text/refusal/function calls and nullable usage. Finish reasons: stop, tool_calls, length for an output limit terminal, content_filter for that incomplete reason. Unrepresentable output/status fails explicitly; hidden reasoning is not rendered as user text.

SSE emits role/text/refusal/function deltas with stable call IDs/indices. Successful completion ends with finish and [DONE]. include_usage adds a final empty-choices usage chunk; preceding chunks have null usage. Stream interruption/conversion failure yields an explicit upstream_interrupted error without successful finish/[DONE]. Already sent deltas cannot be recalled.

Prompt/input and completion/output correspond; cached/reasoning are included subsets. Unknown counters stay null; confirmed zeros stay zero. Nullable usage is accepted by tested SDKs but may need handling in other applications. Projection is bounded to 1 MiB, 1024 output items/content parts; transient output is not written to the database. Terminal text/arguments are checked against emitted deltas by length/hash.

## Responses and errors

HTTP accepts identity JSON and native `Content-Encoding: zstd`, decoded after bearer authentication. Both compressed wire and decoded bodies are bounded to 16 MiB; decoding uses four global slots without queuing, an 8 MiB history window, a ten-second body-read deadline and a two-second cooperative decoding budget checked per 32 KiB output chunk. Decoder slots remain held until blocking work finishes, including after client cancellation. Unsupported/multiple codings return 415 `unsupported_content_encoding`; corrupt or truncated frames return a redacted 400; expansion beyond the limit returns 413 before inference/accounting. This does not force a bearer-authenticated Codex client to enable compression when its own auth gates disable it.

Inference HTTP request bodies and incoming WebSocket messages/frames are bounded to 16 MiB, independently of output/control limits. Oversized HTTP requests return 413 `request_too_large` before inference or usage accounting; malformed JSON retains a redacted `invalid_request_error`. Oversized WebSocket messages close the connection before submission. Configure any HTTP ingress to allow the same request size (the bundled nginx templates do). This byte limit is separate from each model's token context window.

Responses requires full input (string or array) and store=false. Strings become user messages, system becomes developer, absent instructions becomes empty. max_output_tokens returns 400 before inference: the backend rejects output caps. HTTP JSON is assembled from upstream SSE/completed items; raw SSE preserves events. WS does not send the HTTP stream field. Named `stream_id` lanes, saved `conversation`, public `context_management`, background generation and steering are outside this sequential OAuth contract. Unsupported fields/events fail before submission. Responses Lite retains its ordered `additional_tools` prefix, custom and namespace definitions, and uses its required upstream header. An explicit Lite header requires a native Lite prefix or recognized frame-mode metadata; arbitrary caller headers are not forwarded.

Responses preserves message content arrays, including `input_image` with an `image_url` (such as an inline data URL). Codex uses this path for image inputs; image understanding depends on the selected upstream model's capabilities. The router does not fetch, convert or persist images. Chat user `image_url` parts are translated to Responses `input_image` parts, preserving their order among text parts and their URL and optional detail. Image input forwarding does not imply support for every public API image/file format, and the recorded native tool-cycle matrix is not a vision benchmark.

previous_response_id is allowed only for responses owned by the current WS; HTTP/new-socket previous IDs are unsupported. Token validity is checked on each response.create. Opaque encrypted_content from earlier output pins a user-scoped continuation to its account with 24-hour lifetime; unknown/foreign/expired input fails context_not_found, mixed accounts fail context_account_mismatch. Automatic quota failover is the bounded exception to normal account affinity; accepted or ambiguous generations are never replayed. See [account pool](account-pool.md).

Compact adapts one Responses SSE request with compaction_trigger into response.compaction with one opaque checkpoint. Independent generation can select another healthy account, but normal checkpoint/socket affinity wins over cache preference; quota failover requires a complete current context.

Upstream quota cooldown returns upstream_cooldown and retry information. Operational pause returns upstream_backoff. Authentication failure is redacted upstream_authentication_error, distinct from client bearer rejection, with refresh deferred to a separate next operation. Started streams report errors without false success. See [pool](account-pool.md) and [upstream](upstream-contract.md).

Embeddings, image-generation/audio/Realtime endpoints, files/uploads/batches, saved-completion CRUD and Platform API-key upstreams are absent. [Catalog exports/cache behavior](model-catalog.md) and [SDK verification](compatibility.md) define the supported integration scope.

HTTP SSE and native WS streams send a content-free `ping` data event after 15 seconds without a downstream event. Clients should ignore unknown event types. Heartbeats keep the client event reader active; they do not reset the bounded upstream inference read deadline (900 seconds; metadata retains 300 seconds), indicate generation progress or trigger retries. An HTTP upstream interruption emits a generic `error` followed by terminal `response.failed`. A WS interruption emits a wrapped `error` with status 400, type `invalid_request_error` and code `upstream_interrupted`; reviewed Codex treats this as terminal instead of switching to HTTP and resubmitting. The response outcome may still be unknown. This guarantee requires delivery of that error: a lost downstream connection or a shorter client-owned deadline can prevent it. No successful completion or inference replay is invented.

HTTP Responses preserves the upstream `x-codex-turn-state` routing header; WS exposes bounded handshake metadata through a native `response.metadata` event after the first `response.created`, because the upstream account is selected only after the downstream upgrade. This order is required by the reviewed OpenCode sequential driver. Reviewed metadata events also bind turn state before forwarding. New WS frames can carry the issued state in `client_metadata["x-codex-turn-state"]`, or the upgrade header; both transports validate its owning account and project an accepted upgrade value into frame metadata. The router preserves this state for tool continuations within a turn. Only a bounded state previously issued to the same router user is accepted; its keyed digest pins the owning account for 24 hours using the existing bounded context registry. Unknown, foreign or expired state is rejected before inference; conflicting untransferred opaque context is rejected. Quota failover drops the old account-specific turn state and exposes metadata issued by the replacement connection. Raw routing values remain only in transient headers and are never logged or persisted. Failure to bind an HTTP response header omits it while draining and accounting for the accepted response. An unbindable metadata event interrupts forwarding. Additional downstream metadata includes bounded `openai-model`, `x-openai-model`, `x-models-etag`, boolean `x-reasoning-included` and validated numeric `x-codex-primary/secondary-used-percent`, `window-minutes` and `reset-at` headers. The quota fields retain the selected account's actual observations for native HTTP `/status`; missing values and credits are not fabricated. Credentials and arbitrary headers are excluded.

Opaque account affinity includes both `encrypted_content` and the string array `encrypted_function_args` on native function calls. Only separately domain-scoped keyed digests are retained; the values pass upstream unchanged. Empty argument arrays are valid. Unknown or foreign opaque input fails before submission. Mixed untransferred account owners are rejected; derived portability bindings retain authorization across quota transfers and concurrent forks.

Reviewed native session/thread identity fields are scoped consistently in headers, frame metadata and serialized `x-codex-turn-metadata`. Tool inventory and other native turn fields are preserved. Malformed serialized turn metadata is rejected before submission with a fixed error, without echoing its contents.
