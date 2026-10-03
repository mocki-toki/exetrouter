# Native protocol audit — October 2, 2026

This audit compares released client source with ExetRouter. It is a versioned engineering contract, not a promise that the private ChatGPT backend cannot change. A field appearing in a client proves that the client sends it; it does not by itself prove that the server requires it.

## Reviewed sources

| Project | Released revision | Scope |
| --- | --- | --- |
| [Codex](https://github.com/openai/codex/tree/a956835d020762cb2b570053af06f643a11c0ecc) | 0.160.0 | Native request construction, Lite, identity, compression and recovery. |
| [OpenCode V2](https://github.com/anomalyco/opencode/tree/527f0b931d1f9b3ebd34e106c51b31ce5db5b075) | 2.0.22 | Responses lowering, continuation, compaction and ChatGPT adapters. |
| [OpenCode V1](https://github.com/anomalyco/opencode/blob/aec0b9a6d8898f68f923aaf08b7306d931fd9d76/packages/opencode/src/plugin/openai/codex.ts) | 1.18.34 | Separate OAuth/transport implementation; October 3 mock HTTP tool/resume passed. Unmodified client retries errors; provider-scoped V1 bridge passed four one-submission error cases. See [V1 evidence](compatibility.md#opencode-v1-check-2026-10-03). |

Exact commits, selected file hashes and reviewed native test versions live in [protocol-sources.json](protocol-sources.json). Versions were resolved against npm stable tags or GitHub latest releases; release tags were resolved to commits. Development branches are not substituted for releases.

## Keep the three upstream contracts separate

1. Public OpenAI Platform `/v1/responses`: public API authentication, documented state/compaction/WS capabilities.
2. ChatGPT Codex `/backend-api/codex/responses`: Codex OAuth account, native fields and backend-specific transport. This is ExetRouter's upstream.
3. Registered ChatGPT token sharing: separate grant/scope and Platform resource. OpenCode V2's [chatgpt.ts](https://github.com/anomalyco/opencode/blob/527f0b931d1f9b3ebd34e106c51b31ce5db5b075/packages/core/src/plugin/provider/chatgpt.ts) implements this separately from its [Codex OAuth adapter](https://github.com/anomalyco/opencode/blob/527f0b931d1f9b3ebd34e106c51b31ce5db5b075/packages/core/src/plugin/provider/openai.ts). Its credentials and eligibility are not interchangeable with Codex OAuth.

Official [Responses request reference](https://developers.openai.com/api/reference/python/resources/responses/methods/create), [streaming](https://developers.openai.com/api/docs/guides/streaming-responses), [conversation state](https://developers.openai.com/api/docs/guides/conversation-state), [WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode), [function calling](https://developers.openai.com/api/docs/guides/function-calling) and [compaction](https://developers.openai.com/api/docs/guides/compaction) explain the public API. They do not establish support for every public field in the private Codex backend. In particular, public WS named lanes and steering are outside our sequential native WS contract; public stored-response reconnect is outside our `store=false` contract.

## Header policy

Native identity construction is explicit in Codex [requests/headers.rs](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/codex-api/src/requests/headers.rs). Lite, turn state and per-frame metadata are constructed in [core/client.rs](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/core/src/client.rs). Our policy must distinguish authorization, functional continuation and optional client metadata.

| Header / metadata | ExetRouter policy | Current evidence / gap |
| --- | --- | --- |
| `Authorization`, `chatgpt-account-id` | Construct from the selected server-owned OAuth account; never forward the router bearer or client account selection. | Implemented for both transports. |
| `originator`, `User-Agent` | Identify ExetRouter honestly. | Native values differ. Synthetic direct comparisons with our originator/agent completed; exact native impersonation is not established as necessary. |
| `Content-Type`, `Accept` | HTTP JSON request and SSE upstream response. | Implemented. WS removes HTTP `stream`; upgrade headers belong to the transport. |
| `OpenAI-Beta: responses_websockets=2026-02-06` | Set for the selected native WS protocol. | Implemented. Do not equate this protocol with all current public WS extensions. |
| `session-id`, legacy `session_id` | Stable scoped identity; distinguish cache affinity from authorization/account ownership. | Session hints and cache keys are scoped independently when native children share a parent cache. Existing cache digests/account preference remain stable. |
| `thread-id` | Stable scoped thread identity. | HTTP/WS and per-frame hints now have a separate user/model HMAC domain. Parent/child/shared-cache, rotation, user isolation and restart fixtures pass. Impact on the observed stalls is not proven. |
| `x-codex-turn-state` | Preserve an issued opaque token transiently; validate owner before forwarding; persist only its keyed digest. | HTTP headers, WS handshake response.metadata after response.created and event metadata bind issued state before forwarding. WS-to-HTTP/new-WS/restart ownership fixture passes, including upgrade-header projection into a frame. |
| `x-openai-internal-codex-responses-lite` | Enable only for the recognized native Lite shape/mode. | Derived from `additional_tools` or native WS mode metadata. Explicit mode without these witnesses is rejected before submission. |
| `x-codex-beta-features` | Advertise implemented capabilities. | `remote_compaction_v2` implemented; do not advertise arbitrary client features. |
| `x-client-request-id`, parent/subagent/turn metadata | Treat as bounded metadata, never as authorization or a reason to move accounts. | Not forwarded as arbitrary headers. Source presence alone is not proof that each is required for generation. |
| Response model/reasoning/rate-limit headers | Project a reviewed bounded subset when native client behavior needs it; exclude credentials and arbitrary payloads. | Router accounts for quotas; downstream projects bounded model/etag/reasoning metadata and validated turn state. Credentials/arbitrary headers are excluded; dedicated fixture passes. |

## Body and event policy

| Surface | Required handling for the supported contract |
| --- | --- |
| Model and generation options | Preserve the selected catalog model and supported reasoning/text/schema/service-tier options. Never guess context/output limits or silently substitute a model. Reject unsupported output caps. |
| HTTP framing | Decode supported content coding before JSON parsing. Keep wire and decoded-size bounds, decompression CPU/concurrency and error redaction independent of token windows. Authenticated identity/zstd decoding is implemented with wire/output, history-window, slot and cooperative CPU bounds; malformed/truncated/expansion fixtures pass. |
| HTTP Responses | Normalize string input to a user item and message `system` to `developer`; preserve item order/content. Force upstream `store=false`, `stream=true`; project JSON only when requested downstream. |
| Native standard Responses | Preserve tools, tool choice, parallel-tool flag, include, reasoning and text controls. Preserve unknown native item extensions transiently rather than rebuilding them as Chat messages. |
| Responses Lite | Preserve `additional_tools` and its ordered developer prefix, namespaced/custom tools and rebuilt instruction items. Do not flatten this into classic function tools. |
| Tool cycle | Preserve `call_id`, name/namespace, function `arguments`, custom tool `input`, matching outputs and ordering. `custom_tool_call` is not an empty-JSON `function_call`. Never invent successful tool results to repair a history. |
| Reasoning/history | Preserve complete output items, assistant `phase`, encrypted reasoning/compaction and `encrypted_function_args`. Bind opaque values to their owner before exposing them; never decode them or persist their payload. |
| Native WS | Sequential `response.create`, same-socket `previous_response_id`, generated-false warmup and account ownership. Remove HTTP `stream`. A new socket needs full context and validated opaque ownership. |
| Streaming | Preserve raw event bytes, order, IDs, deltas, item-done and terminal events. Fragments may split UTF-8/JSON or SSE delimiters. A tool-input delta or socket EOF is not completion. |
| Failure | Terminal failure/incomplete remains distinct from success. Interrupted usage remains unknown unless counters were confirmed. Keep bounded upstream deadlines even when downstream receives content-free heartbeats. |
| Compaction | Our compact endpoint adds `compaction_trigger` to one upstream Responses request and projects its checkpoint. OpenCode V2 supports this trigger separately from a standalone compact endpoint. Do not substitute the public compact algorithm or assume it returns only one item. |

Codex [common.rs](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/codex-api/src/common.rs) and [client.rs](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/core/src/client.rs) define these native fields; [http-client/request.rs](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/http-client/src/request.rs) implements `zstd` body encoding. OpenCode's [Responses lowering](https://github.com/anomalyco/opencode/blob/527f0b931d1f9b3ebd34e106c51b31ce5db5b075/packages/ai/src/protocols/openai-responses.ts) and [continuation driver](https://github.com/anomalyco/opencode/blob/527f0b931d1f9b3ebd34e106c51b31ce5db5b075/packages/ai/src/protocols/open-responses-continuation.ts) preserve complete history and only send an incremental suffix after checking the preceding request/output.

The additional pinned [responses_metadata.rs](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/core/src/responses_metadata.rs) defines duplicate session/thread identities inside serialized turn metadata, alongside tool namespace inventory. ExetRouter now projects the same scoped identities there and in WS continuations; its earlier header/frame-only projection was inconsistent. This fixes a concrete contract mismatch. It does not independently prove the cause of an observed backend stall.

## Recovery is a separate contract

Codex [responses_retry.rs](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/core/src/responses_retry.rs) contains a WS-to-HTTP fallback after the configured stream retry budget is exhausted, conditional on a retryable error and transport eligibility. Thus `stream_max_retries=0` is not proof that every native client recovery path submits only once. Our six current native post-submission WS fixtures now count one generation for early/partial/missing-terminal failures. The router emits a wrapped terminal 400 recognized by Codex. A lost downstream connection can still prevent delivery of that error; the router cannot control every client recovery path.

## Prioritized gaps and acceptance

All implementation rows below are complete except the sustained-session acceptance row. Both reviewed clients passed two individually observed compact/resume cycles over public HTTP/WS ingress, with five native process invocations and a verified router restart per case. A separate Codex WS case reached 217,156 observed input tokens and retained its canary after compaction. Full advertised windows, multi-hour sessions and saturation remain pending.

| Priority | Work | Acceptance evidence |
| --- | --- | --- |
| P1 | Bounded `zstd` request decoding; explicit unsupported-coding errors. | Valid compressed requests reach upstream once; corrupt/truncated/oversized/bomb inputs fail before inference, without payload logs. Native compression no longer needs disabling. |
| P1 | Separate scoped thread identity from cache affinity. | Root, child and shared-cache threads retain native distinctions; user isolation, token rotation, HTTP/WS and restart remain stable. No raw identity retention. |
| P1 | Test client recovery after a submitted WS interruption and cross-transport turn ownership. | Count actual downstream/upstream submissions; injected early/late close, idle timeout and absent terminal never produce hidden replay or account migration. Resolve any client recovery limitation explicitly. |
| P1 | Sustained current-client sessions and compaction. | Both clients passed two observed cycles, reopen/restart and external ingress. Codex WS also passed a 217k-token case. Full advertised context, large tool outputs, multi-hour sessions and saturation remain separate acceptance work. |
| P2 | Explicit capability errors for unsupported public WS/state features and header negotiation. | Named lanes/steering/HTTP saved-response chaining are either implemented and tested or rejected before submission. Lite and native response metadata have dedicated fixtures. |
| P2 | Broaden current native fixtures. | Custom/freeform/namespace tools, child agents, phase, encrypted function arguments, multimodal forwarding and interleaved reasoning each have meaningful fixtures. Model understanding remains separate from byte forwarding. |

## Evidence and repeatable review

- Offline native runs on 2026-10-02: Codex 0.160.0 and OpenCode V2 2.0.22 each completed tool → local execution → tool output → final response over HTTP/SSE and WS against the mock. Both HTTP 503 cases made one primary submission. Six submitted WS interruption cases made one generation each, with no HTTP replay. All 206 ordinary tests and standard static/publication checks passed.
- Authorized current-client real tool cycles passed on Linux aarch64 over HTTP/WS, plus one explicit compressed HTTP request. OpenCode exposed an ordering regression in the first WS probe; moving handshake metadata after response.created passed a new independent real cycle. A five-process Codex WS canary/reopen/restart probe reached 49–86k input tokens with one correctly unknown interrupted warmup. Two lowered compaction thresholds were used, but actual compaction requests were not counted. These results do not establish sustained/full-window reliability or replace the stronger four-case harness; see [measured results](live-testing.md#measured-follow-up-2026-10-02).
- Previously authorized real-backend Codex 0.160.0 synthetic probes after the thread-header fix completed a 100-line patch at about 35k and 120k input tokens over HTTP and a Code Mode call at about 120k over WS. All reached completion in 20–31 seconds. Public ingress and multi-hour sessions were not tested by those probes.
- The earlier stream stalled inside custom-tool input, before a terminal event. The header fix was followed by passing reproduction scenarios. This supports the fix; the exact private-backend causal mechanism is not independently proven.
- The later public-ingress matrix passed Codex HTTP/WS and OpenCode HTTP/WS through two observed compaction cycles, process reopening and verified server restart. Before serialized turn identity projection, a separate Codex WS case stalled after compaction and retained one interrupted request with unknown usage; its fixed event timings do not prove the private-backend cause. After projection, all four cases passed with fresh sessions. The independent large Codex WS case reached 217,156 input tokens, compacted and completed a canary tool continuation with six known completions. These bounded results and the 192k-token fixture that fell below its test-size requirement are recorded in [deployment evidence](live-testing.md#measured-deployment-continuity-2026-10-02).

Review source drift explicitly:

```sh
python3 scripts/check-protocol-sources.py --fetch --check-current
python3 scripts/check-protocol-sources.py
```

The first command downloads only pinned public reference files, verifies SHA-256 and compares current stable release versions/commits. The second verifies the cached references offline. Neither performs inference or executes downloaded code. A version or tag change fails the current check; inspect the changed source and update the manifest deliberately. The Rust native fixture reads its expected versions from that reviewed manifest, not registry `latest` at test time.

Then fetch verified client runtimes and run the isolated mock matrix as documented in [compatibility.md](compatibility.md). Live tests remain separately authorized and bounded under [live-testing.md](live-testing.md). Review both success and failure traces with synthetic inputs; never capture real conversations or credential-bearing traffic. Maintain the actual support contract in [openai-api.md](openai-api.md) and [upstream-contract.md](upstream-contract.md), and track completion in [development-plan.md](development-plan.md).
