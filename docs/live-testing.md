# Real upstream testing

Use dedicated operator OAuth accounts and private state outside build output. Ordinary cargo test never contacts real upstream. The six tests in `tests/live_upstream.rs` are ignored: `live_catalog`, `live_upstream_smoke`, `live_native_clients`, `live_native_compaction`, `live_prompt_cache`, `live_two_account_pool`. Deployed fixtures are also opt-in. Inference consumes the selected account's quota.

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

Protocol smoke: 11 completed/known usage. The October 1 native matrix: Codex HTTP 2, WS 3 including warmup; OpenCode HTTP/WS 3 each including title; all known in the latest run. Earlier cancelled title requests had unknown usage.

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

At this point, Linux/aarch64 and public ingress were not verified; later runs below add that evidence. Full model windows, multi-hour/load and actual external authentication/quota recovery remain unverified.

## Measured follow-up, 2026-10-02

Codex 0.160.0 and OpenCode V2 2.0.22 ran on Linux aarch64 with isolated private profiles and loopback ingress. HTTP tool cycles completed in 18 seconds/two requests (Codex) and 14 seconds/three requests (OpenCode); Codex WS took 29 seconds/three requests. OpenCode WS initially failed because handshake metadata preceded `response.created`. After correcting the order, a new independent cycle completed in 14 seconds/three known requests, without HTTP fallback. One explicit compressed HTTP generation also completed with known usage.

A separate Codex WS probe retained a seed canary across five process invocations, fresh reference text and verified backup/router restart. Measured input grew from 49k to 86k tokens; request counts were 2/5/3/4/3, with one interrupted warmup correctly unknown. Two thresholds were lowered, but actual compaction requests were not counted. It therefore does not replace the four-case compaction harness.

Separate native custom-tool probes after the thread-header fix completed a 100-line patch over HTTP at about 35k/120k input tokens and a Code Mode call over WS at about 120k. All reached completion in 20–31 seconds. The earlier stall occurred during custom-tool input; passing reproductions support the correction without proving a private-backend cause.

These were bounded loopback checks. An external TLS check returned 404 for unexposed health routes; public inference was verified only by the later matrix below. Restricted SSH and loopback health were checked separately during installation.

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

Codex 0.160.0 and OpenCode V2 2.0.22 ran on macOS arm64 through public HTTPS/WS to a Linux aarch64 router. Every case used a fresh private profile/temporary token, five process invocations, two individually observed compaction cycles and tool-based recall of a seed canary. The supervisor verified a drained private backup, restarted the existing service and checked health between cycles. Tokens were revoked afterward; no production refresh-token database was copied.

| Case | Initial input tokens | Observed compaction requests | Durable requests / known usage | Continuity and supervised restart |
| --- | ---: | ---: | ---: | --- |
| Codex HTTP | 25,680 | 4 | 13 / 13 | Passed |
| Codex WS | 25,552 | 4 | 18 / 18 | Passed |
| OpenCode HTTP | 17,430 | 2 | 12 / 12 | Passed |
| OpenCode WS | 17,341 | 2 | 12 / 12 | Passed |

All requests completed on one owning account per case. Codex made two compaction requests per forced cycle, OpenCode one; counts include warmups/title requests. WS checkpoint tools stayed on WS; OpenCode's auxiliary HTTP title was expected. Public catalog routes returned authenticated 200/unauthenticated 401. Health routes intentionally returned 404. Existing restricted/administrative SSH configuration was preserved.

A pre-fix Codex WS case stalled after compaction: six known completions and one interrupted unknown request, with about 139 seconds since the last upstream event. Serialized turn metadata had inconsistent session/thread projection; all four fresh cases passed after correcting it. An earlier HTTP case had already passed with 25,587 initial tokens/13 known requests. No possibly submitted generation was replayed, and these observations do not prove the backend cause.

The large-context Codex WS case reached **217,156 input tokens**, one observed compaction, six known completions and retained tool-based canary recall after reopening, with no HTTP generation fallback. Process durations were about 9/24 seconds. An earlier 192,477-token case completed the same mechanism but failed the harness's 200,000-token size requirement. The later larger fixture was independent input.

Two supervisor attempts stalled before inference because child stdin remained open; both had zero durable requests. Closing child stdin corrected the harness.

The matrix verifies bounded public-ingress continuity, not full advertised context, large custom-tool output, multi-hour sessions, saturation or actual authentication/quota outages. The optional multi-hour idle mode was not executed. The isolated two-cycle harness was compiled, not run against a copied production grant pool.

## Cross-account continuity and synthetic quota failover, 2026-10-02

Direct upstream probes used two eligible accounts, synthetic input and no automatic retries:

| Probe | Observed input / output tokens | Result |
| --- | --- | --- |
| HTTP checkpoint on A → classic function on B → result on B | 11,948 / 77; 156 / 33; 181 / 12 | Hidden seed canary retained |
| WS checkpoint and function on A → full checkpoint/call/result on B | 11,942 / 102; 246 / 37; 200 / 31 | Seed canary and smoke marker retained without seed or previous response ID on B |
| Minimal classic WS control | 139 / 23 | Completed |

These direct calls were outside router accounting. The WS case returned no `encrypted_function_args`, so it does not verify their live portability. An initial compaction diagnostic failed to assemble omitted terminal items; later direct WS Lite probes were rejected before completion. Those failures were not replayed or counted as successful evidence.

Mock Codex 0.160.0 and OpenCode V2 2.0.22 each continued HTTP/WS tool results through a synthetic refusal on A and acceptance on B. Total request counts were Codex 3/4 and OpenCode 4/4, including warmups/title; each had one refused attempt. Six early-close/partial/missing-terminal cases still made one primary submission without HTTP replay.

Run the new synthetic native matrix with the reviewed isolated binaries:

```sh
cargo test --locked --test responses current_clients_continue_tools_across_a_quota_account_switch -- --ignored --nocapture
```

This mock test consumes no real quota. Actual external quota exhaustion was not induced. Live WS encrypted-tool transfer, multi-hour/concurrent failover, unavailable recovery windows and all upstream refusal variants require separate evidence.

An isolated real-upstream router fixture then completed six requests with known usage and account sequence **A/A/B/A/B/B**. WS compacted/called a tool on A, then a synthetic local cooldown transferred its incremental result to B on the same downstream socket. HTTP compacted on A, then a current-100% test observation selected B for a canary function/result cycle. WS stayed on WS. No old turn-state header appeared, so transfer stripping remains synthetic-fixture evidence.

Only access tokens were encrypted into fresh tmpfs state with synthetic unusable refresh grants. Production refresh grants, preferences, cooldowns and keys were preserved; temporary state/container were removed. Earlier bridge-network probes each left one interrupted unknown request. A host-network fixture then stopped after three WS requests and HTTP compaction because of a synthetic quota-row uniqueness error. A new upsert-based fixture completed the six-request matrix; none of the failures caused replay. Validation passed 213 offline tests and standard checks.

The same source was installed after a verified private backup and zero-pending check. All 46 build inputs matched; gateway, keys and host-network settings were preserved, and health passed. A public current-client HTTP/WS tool matrix completed eleven known requests without injecting production quota changes. Its temporary bearer was revoked. The matching Mac client was replaced atomically, preserving profiles/keys. These were development builds of 0.1.0 at the time; no release tag was published by that operation.

A final header fixture verified that an A refusal at 100%, followed by B acceptance at 20%, returns B's 20% observation for native HTTP `/status`. Unknown limits remain unknown; the synthetic switch matrix and standard checks passed afterward.

Actual subscription exhaustion was not induced. Live WS encrypted-tool portability, multi-hour/concurrent failover, unavailable recovery windows and all upstream refusal variants still require evidence.
