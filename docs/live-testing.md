# Real upstream testing

Use dedicated operator OAuth accounts and private state outside build output. Ordinary cargo test never contacts real upstream. The six real tests are ignored: live_catalog, live_upstream_smoke, live_native_clients, live_native_compaction, live_prompt_cache, live_two_account_pool. Inference consumes the selected account's quota.

## Preparation

```sh
cargo build --locked --bins
mkdir -p .local-state
chmod 700 .local-state
mkdir -m 700 .local-state/upstream-smoke
export EXETROUTER_LIVE_STATE="$PWD/.local-state/upstream-smoke"
(cd "$EXETROUTER_LIVE_STATE" && ../../target/debug/exrd init)
(cd "$EXETROUTER_LIVE_STATE" && ../../target/debug/exrd admin oauth add --device)
```

The operator completes device login; credentials are received/encrypted directly, never sent in chat/arguments. Keep state private and ignored. The service must be stopped, with no control socket/pending request. One-account tests require exactly one active account; pool requires two. Native clients accept two only with EXETROUTER_LIVE_ACCOUNTS=2. Run tests sharing state sequentially.

```sh
cargo test --locked --test live_upstream live_catalog -- --ignored --nocapture
export EXETROUTER_LIVE_MODEL='<MODEL_ID_FROM_CATALOG>'
cargo test --locked --test live_upstream live_upstream_smoke -- --ignored --nocapture
```

Protocol smoke makes 11 requests: Responses JSON/SSE, forced function call/result, compact/checkpoint continuation, sequential WS previous_response_id, Chat JSON/SSE and a request after actual leased refresh. Assertions compare terminal usage/accounting/transport. No automatic retry on failure. EXETROUTER_LIVE_ONLY=http_json or chat_refresh selects a new independent diagnostic request.

## Native clients

Resolve/download/review current clients using [compatibility](compatibility.md). Set EXETROUTER_CODEX_BIN and EXETROUTER_OPENCODE_BIN from target/compat/current-clients.json. Use the full Codex runtime.

```sh
cargo test --locked --test live_upstream live_native_clients -- --ignored --nocapture
```

Four isolated HTTP/WS profiles receive exported real catalogs, run /bin/echo EXETROUTER_TOOL_OK in a temporary workspace, and return its result. Assertions inspect the actual tool cycle, primary transport and durable usage. OpenCode's title may be auxiliary HTTP and may be cancelled with unknown usage. Codex retries are disabled; OpenCode uses the built-in `openai` provider and its standard retry policy. Ordinary client auth/settings are untouched.

The harness creates/revokes a temporary bearer, stops its local service and preserves OAuth state/usage. Budget: 12 inference requests and 180 seconds per short scenario. EXETROUTER_LIVE_CLIENT selects codex-http/codex-ws/opencode-http/opencode-ws.

## Two accounts

Add a different second test account while the service is stopped; signing into the same account updates its record. Choose a model available to both.

```sh
cargo test --locked --test live_upstream live_two_account_pool -- --ignored --nocapture
EXETROUTER_LIVE_ACCOUNTS=2 cargo test --locked --test live_upstream live_native_clients \
  -- --ignored --nocapture
```

The pool fixture makes ten requests: both account seeds, compaction on A, checkpoint continuations with a cache preference for B, WS continuation, independent B requests while A is synthetically paused, then original WS/new A recovery. A random continuity token appears only in the seed and must survive. Three local requests reject cache-key change/pinned continuation during pause without new usage.

The synthetic responses-health pause is bounded to 300 seconds and restored only if still owned by the fixture; newer real observations are preserved. This tests routing around a local pause, not an actual upstream 429/outage or subscription exhaustion.

## Automatic native compaction

```sh
cargo test --locked --test live_upstream live_native_compaction -- --ignored --nocapture
```

Four HTTP/WS scenarios create a 17–25k-token synthetic history with a random continuity token, reopen the session, trigger native compaction and require a local command recalling the token; a third process reopens and verifies it again. Later prompts do not repeat the token. Compaction threshold is 512 below measured initial input, while catalog limits remain actual. Budget is 24 requests/180 seconds per scenario. This tests the native mechanism, not full-context/multi-hour reliability.

## Prompt cache

```sh
cargo test --locked --test live_upstream live_prompt_cache -- --ignored --nocapture
```

Six HTTP JSON requests, three with explicit key and three without, use separate fresh prefixes. Input/cached/output and timing are compared to durable accounting. Unknown counters stay unknown; zero hits are valid observations. Requests are not repeated to obtain a favorable result.

## Measured results, 2026-10-01

Protocol smoke: 11 completed/known usage. Latest native matrix: Codex HTTP 2, WS 3 including warmup; OpenCode HTTP/WS 3 each including title; all known in the latest run. Earlier cancelled title requests had unknown usage.

Automatic compaction:

| Scenario | Initial input | Test threshold | Compaction requests | Total / known usage |
| --- | ---: | ---: | ---: | --- |
| Codex HTTP | 25564 | 25052 | 2 | 7 / 7 |
| Codex WS | 25489 | 24977 | 2 | 10 / 10 |
| OpenCode HTTP | 17224 | 16712 | 1 | 7 / 7 |
| OpenCode WS | 17275 | 16763 | 1 | 7 / 7 |

All preserved continuity and completed tool cycles after reopening. Counts may vary with client behavior.

For the stronger two-cycle/restart fixture, set `EXETROUTER_LIVE_TWO_COMPACTIONS=1` and run `live_native_compaction` against the isolated, stopped test service. Optional `EXETROUTER_LIVE_ACCOUNTS=2` admits an isolated two-account pool. Each case adds fresh disposable reference text after the first checkpoint, restarts the router before the second compact/resume cycle, checks the original canary through tool output after reopening, and requires at least two observed compaction requests. The budget is 36 requests with 180 seconds per native process. This is a bounded continuity test, not a multi-hour or full-window benchmark; never run a copied refresh-token pool alongside its original service.

Cache observation: explicit-key group input 6646 each, cached 0/0/6400 (32.10% aggregate, 96.30% third request); no-key group input 7046 each, cached 0/0/0. All six completed and matched SQLite; reported cache-write counter was zero. Different prefixes mean these are not causal evidence that explicit keys outperform default keys, nor a subscription savings guarantee.

Two-account pool: ten known completions, A seven/B three, three local rejections, preserved checkpoint and original WS. Client-export failure from differing supports_experimental_context was fixed with conservative capability intersection before the successful run. The repeated native matrix on two accounts completed another eleven known requests using both accounts.

Full model windows, sustained sessions/load, actual external authentication/quota recovery, Linux/aarch64, public TLS/nginx and restricted SSH UID remain unverified. Short local real-upstream runs do not establish deployment readiness.

## Measured follow-up, 2026-10-02

Reviewed current runtimes were Codex 0.160.0 and OpenCode V2 2.0.22. Authorized Linux aarch64 probes used temporary private profiles, loopback ingress, revoked diagnostic tokens and no inference retries. HTTP tool cycles completed in 18 seconds (Codex, two requests) and 14 seconds (OpenCode, three requests). Codex WS completed in 29 seconds with three requests. The initial OpenCode WS probe failed because router handshake metadata preceded `response.created`; after correcting the order, a new independent probe completed its tool cycle in 14 seconds with three completed requests and no HTTP fallback. This was not a replay of the failed generation. An explicit compressed HTTP probe completed one generation with known counters.

A separate Codex WS continuity probe used five native process invocations. The original random canary appeared only in the seed; later invocations had to recall it through a local tool. Fresh reference text increased measured input from about 49k to 86k tokens. The router was restarted after a verified private backup between the third and fourth invocations. Request counts were 2/5/3/4/3; all generations completed except one interrupted warmup with correctly unknown usage. Both later tool checks retained the canary, and there was no HTTP fallback. Two thresholds were lowered to request native compaction, but the diagnostic did not count actual compaction requests. The strengthened four-scenario `EXETROUTER_LIVE_TWO_COMPACTIONS=1` harness was compiled, not executed in this follow-up.

These results cover bounded current-client tool and continuity scenarios, not full model windows, multi-hour sessions, all-client repeated compaction, saturation or external ingress inference. A separate external HTTPS check verified TLS but found the health routes unexposed (404); it does not verify public HTTP/WS sessions. Restricted SSH service and loopback API health were checked separately during installation.

## Current clients through an existing deployment

`deployed_native_continuity` uses the running server's normal API and account pool; it does not open a copied OAuth database. Set `EXETROUTER_DEPLOY_URL` to the verified HTTPS origin, `EXETROUTER_DEPLOY_MODEL` to a catalog model and the restricted SSH `EXETROUTER_DEPLOY_HOST`/`EXETROUTER_DEPLOY_IDENTITY` settings. Optional `EXETROUTER_DEPLOY_USER` and `EXETROUTER_DEPLOY_PORT` preserve an operator's existing SSH endpoint. The harness creates/revokes a temporary router token through that gateway.

```sh
cargo test --locked --test deployed deployed_native_continuity \
  -- --ignored --nocapture --test-threads=1
```

`EXETROUTER_DEPLOY_CLIENT` selects `codex-http`, `codex-ws`, `opencode-http` or `opencode-ws`; omission runs all four. Each case has five separate native processes, fresh reference text, two individually observed compact/resume cycles, a retained canary and a 36-request limit. Usage observations retain only numeric counters; requests, outputs and credentials remain transient. The loopback observer forwards reviewed session/thread/turn/Lite headers and observes bounded JSON/SSE/WS terminal usage, including fragmented SSE and compressed request bodies. Native process stdin is closed explicitly so an operator's supervision pipe cannot stall Codex before submission.

If management is supervised externally, `EXETROUTER_DEPLOY_BEARER` accepts a temporary token through the process environment. Its caller owns creation and revocation in a `finally` block; never put it in arguments, shell history or published config. `EXETROUTER_DEPLOY_RESTART_STDIN=1` requests a supervised restart before the second cycle: the test emits `restart_required` and waits up to 90 seconds for one `R` byte on stdin. Send it only after checking for active requests, creating/verifying a private backup, restarting the existing service and checking health. Any other byte or deadline aborts that case without replay. Without this setting, the fixture verifies reopen/compaction only and reports `restart_supervised=false`.

`EXETROUTER_DEPLOY_LARGE_CONTEXT=1` requires one selected case and substitutes two process invocations with 2,500 disposable reference rows, a 150,000-token compaction threshold and a 12-request budget. Acceptance requires at least 200,000 input tokens observed in terminal usage and a checkpoint tool continuation. It does not perform the two-cycle restart scenario. `EXETROUTER_DEPLOY_IDLE_SECONDS` optionally pauses before the final process for 0–7,200 seconds; this tests reopening after idle, not preservation of an open socket. Both modes retain the 180-second deadline per native invocation. A configured idle interval is not evidence that a multi-hour run passed.

## Measured deployment continuity, 2026-10-02

Reviewed Codex 0.160.0 and OpenCode V2 2.0.22 ran on macOS arm64 through public HTTPS/WS to the existing Linux aarch64 router. Each case used a fresh private profile and temporary token, five native process invocations, disposable reference text, two separately observed compaction cycles and tool-based recall of the original random canary. Between cycles, the operator verified a drained private backup, restarted the existing router service and checked health. Tokens were revoked afterward. The existing production pool was used directly; no refresh-token database was copied.

| Case | Initial input tokens | Observed compaction requests | Durable requests / known usage | Continuity and supervised restart |
| --- | ---: | ---: | ---: | --- |
| Codex HTTP | 25,680 | 4 | 13 / 13 | Passed |
| Codex WS | 25,552 | 4 | 18 / 18 | Passed |
| OpenCode HTTP | 17,430 | 2 | 12 / 12 | Passed |
| OpenCode WS | 17,341 | 2 | 12 / 12 | Passed |

All requests completed; each case stayed on one owning account. Codex made two compaction requests in each forced cycle, OpenCode one. Counts include native warmups and auxiliary title requests where present. WS checkpoint tool results used WS; OpenCode's auxiliary HTTP title was expected. Public catalog routes returned authenticated 200 and unauthenticated 401. Public health routes remain intentionally unexposed, so their 404 is not an inference failure. Restricted SSH was not reconfigured; the supervisor used existing administrative SSH to the registered restricted gateway identity.

Before the serialized turn identity fix, a separate Codex WS case stalled after compaction: six requests completed with known counters and one interrupted request retained unknown usage. The last allowlisted event timing showed about 139 seconds without a new upstream event before the native process deadline ended the stream. No possibly submitted generation was replayed. Exact native source revealed inconsistent session/thread projection inside serialized turn metadata; the router now scopes it consistently with headers and frames. An earlier Codex HTTP case passed before the correction at 25,587 initial input tokens with 13 known requests. All four cases in the table passed afterward with fresh sessions. This supports the corrected contract but does not prove the private-backend cause of the stall.

The separate large-context Codex WS mode reached 217,156 observed input tokens. It compacted, reopened the session and recalled the seed canary through a local tool: one observed compaction request, six completed requests with known counters, one account, no HTTP generation fallback. Native invocation durations were about 9 and 24 seconds. An earlier independently completed 192,477-token case passed the same mechanism with six known requests but failed the harness's 200,000-token size requirement; increasing the next case's disposable reference fixture satisfied that requirement. This was new synthetic input, not a replay.

Two earlier supervisor attempts inherited an open stdin pipe in the native child and waited before submitting inference; both had zero durable inference requests. Explicitly closing native child stdin fixed the fixture. Fixed counters now distinguish request receipt, body decoding, forwarding, response headers and terminal usage without exposing content.

These results verify bounded public-ingress continuity and repeated native compaction for the reviewed clients. They do not establish the full advertised model window, large custom-tool output, multi-hour sessions, saturation or actual external authentication/quota recovery. The optional multi-hour idle mode was implemented but not executed in this matrix. The standalone two-cycle harness was compiled, not run against a copied production grant pool.

## Cross-account continuity and synthetic quota failover, 2026-10-02

An authorized direct HTTP diagnostic used two eligible accounts, fresh synthetic identifiers and no automatic retries. Account A produced one encrypted compaction checkpoint (11,948 input / 77 output tokens). Account B received that checkpoint without the original seed, recalled the hidden identifier through a forced classic function call (156 / 33), then completed its tool-result continuation (181 / 12). This establishes portability for that checkpoint/classic-tool case on the private backend, rather than an assumption from Platform documentation. The diagnostic contacted upstream directly, outside router durable accounting; it printed only fixed proof flags and numeric token counters.

A subsequent direct classic WS case also passed: A produced a checkpoint (11,942 / 102), recalled the hidden identifier through a WS function call (246 / 37), and B accepted the full checkpoint/call/result window and returned both the original identifier and the smoke marker (200 / 31). No original seed or previous response ID was sent to B. This case returned no `encrypted_function_args`, so it does not establish their live portability. A minimal classic WS control completed separately (139 / 23).

A separate initial compaction probe completed but its diagnostic failed to assemble items omitted from terminal output. Subsequent direct WS Lite diagnostics were rejected before completion; changing the synthetic fixture did not establish WS encrypted-tool portability. These failures were not retried as an inference operation and are not recorded as successful compatibility evidence.

Offline native binaries Codex 0.160.0 and OpenCode V2 2.0.22 each passed HTTP/WS tool continuation through a synthetic quota rejection on A followed by acceptance on B. Their total request counts were respectively 3/4 and 4/4, including warmup/title where present, with one rejected attempt and one accepted tool-result continuation per case. Six native early-close/partial/missing-terminal cases still made one primary submission each, without HTTP replay.

Run the new synthetic native matrix with the reviewed isolated binaries:

```sh
cargo test --locked --test responses current_clients_continue_tools_across_a_quota_account_switch -- --ignored --nocapture
```

This mock test consumes no real quota. Actual external quota exhaustion was not induced. Live WS encrypted-tool transfer, multi-hour/concurrent failover, unavailable recovery windows and all upstream refusal variants require separate evidence.

The newly built router image subsequently passed an isolated real-upstream quota-transfer fixture with six completed/known requests. Only access tokens for two eligible accounts were encrypted into fresh tmpfs state with synthetic unusable refresh grants; real refresh grants, production preferences, cooldowns and keys were not copied or changed. WS compacted on A, issued a tool call on A, then used a local test cooldown to continue the incremental tool result on B without changing the downstream socket. HTTP compacted on A, then used a local current-100% observation to select B for the canary tool/result cycle. The durable account sequence was A/A/B/A/B/B; both WS generations remained WS. Temporary container/state were removed. No old turn-state header was issued in that live WS case, so its transfer stripping remains covered by protocol fixtures rather than this live observation.

Two earlier isolated probes could not contact upstream from the temporary bridge-network container and recorded one interrupted request with unknown usage each. The fixture was changed to the deployment's existing host network with a private loopback listener. Its next run completed the three WS requests and an HTTP compaction, then stopped at a synthetic quota-row uniqueness error before further inference. A new independent fixture with an upsert completed all six requests above. None of these failures triggered inference replay. Ordinary validation after the implementation passed 213 offline tests and the standard static/publication checks.

The same runtime source was installed on the existing server after a verified private backup and a zero-pending-request check. All 46 build inputs matched the workspace, the restricted gateway matched the server binary, keys and the existing host-network configuration were preserved, and health remained green. A public TLS current-client tool matrix completed eleven requests with eleven known usage records and no interruption/failure. It covered Codex 0.160.0 and OpenCode V2 2.0.22 over HTTP and WS; it did not inject quota changes into production. The temporary bearer was revoked. Mac `exr` was replaced atomically with the matching local build, without changing profiles or keys. Both remain development builds of 0.1.0; no release tag was published.

The final quota-header fix passes only validated numeric upstream window fields to HTTP clients. A synthetic A refusal reporting 100% followed by B acceptance reporting 20% returned B's 20% header. This preserves native HTTP `/status` observations after a switch; unknown limits are not fabricated. The current-client synthetic quota-switch matrix and standard checks also passed after this change.
