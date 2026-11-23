# Subscription history

Account details in the native TUI and OpenCode Desktop show short and weekly
subscription windows together, with separate colors, a shared 0–100% quota-left
axis. The native TUI shows seven days; OpenCode Desktop offers rolling **24 hours,
7 days, 30 days and 90 days**, with arrows to earlier/later intervals. The web chart
supports pointer/keyboard selection; TUI details use Left/Right and Home/End, with Up/Down for scrolling.
Lines and cursor values show only the remaining percentage: a full window is
100%, an exhausted window is 0%, and quota recovery moves the line upward.
Storage keeps upstream used percentages; display converts them without changing
the collected percentages or routing behavior.
Usage charts use the same quiet containers, fine lines, distinct identity-based
colors and interval values (Shift+Left/Right and Home/End in the TUI, without
changing Left/Right tab navigation).

The router stores the **latest accepted numeric observation in each UTC ten-minute interval**,
not an average, token-derived estimate or historical upstream query. Both
general-meter kinds retain their actual duration, reset time and observation
time. A new quota cycle is not smoothed away. Charts leave missing intervals empty
and do not connect across gaps or duration changes. Display dates use the
client's local time zone; a tooltip identifies its bucket, not an exact
measurement at its start. The current bucket can change in the background.

Collection runs wherever quota observations already arrive: HTTP/WS metadata
and the existing passive limits worker. No new inference, metadata poll, OAuth
operation or reset-credit action is introduced. It works while dashboards are
closed and survives restart. History starts after upgrade; old days cannot be
reconstructed. Updated clients also accept older routers' hourly history. Older
clients require an update for ten-minute history; current account details remain
available in the native TUI.

Authenticated Doctor/Limits expose optional `quota_accounts[].quota_history`
with `from_utc`, exclusive `to_utc`, `interval_seconds: 600` and bounded `points`.
Each point includes `kind`, `hour`, `observed_at`, `used_percent`, nullable
`window_minutes` and nullable `reset_at`. The legacy name `hour` now denotes the
start of a ten-minute bucket. The current bucket and preceding 1,007 buckets
are returned, up to 2,016 points per account; no client identities, request
contents or credentials are retained in history. Capability
`ten_minute_quota_history: 1` identifies the new resolution; the legacy
`hourly_quota_history: 1` collection flag is retained. Ordinary CLI reports
remain text-only; JSON is the automation interface.

`exr quota-history ACCOUNT --period 90d --json` reads a wider interval through the
authenticated management channel, without refreshing upstream. `--before UNIX`
sets its exclusive, ten-minute-aligned end; subtract the selected duration to page back.
Each response contains at most 90 days / 25,920 points. Desktop paging needs updated
native and server binaries; an older router's seven-day snapshot remains visible.
Numeric observations are retained for at most **365 days**, up to 52,560 rows per
account/window. Previously pruned observations cannot be recovered; retention expands
only as new observations arrive. Rolling windows have fixed durations even
across DST. Collection still uses the existing roughly 30-second passive worker;
changing bucket size adds no polls.

On request, Desktop adds a Platform API line to the same percent-remaining chart.
It uses the current positive manual balance and ExetRouter's recorded Platform
request costs, not organization-wide daily costs. The chart reads sparse changes
in ten-minute UTC buckets, carries earlier spend into older pages, and stops at
the first unpriced request. A new balance entry restarts the series. The optional
`platform_balance` in `quota-history --json` requires updated router code (or an
updated embedded runtime in standalone); older responses still show Codex.

## History-based depletion forecasts

The shared runtime derives optional `quota_accounts[].quota_forecast` metadata
for TUI, Desktop and human-readable Limits. This predicts **quota depletion**,
not the expiry or renewal of a paid subscription. It is display-only: ranking,
affinity, admission and automatic reset-credit eligibility do not read it.
No schema migration, extra upstream poll, inference or persistent forecast state
is introduced. The existing account-scoped history query reads at most 8,642
numeric points over the previous 30 days; it does not send that wider history
to clients. Browsing an older chart interval does not change the forecast.

OpenCode Desktop shows one compact current ETA and averages the two depletion
times when both are available. It caches the value per snapshot, hides it when
expired or disconnected, and does not show a historical-only ETA or explanatory
pace labels. The separate labels below describe the terminal clients.

Two estimates remain distinct:

- **Current pace** uses a contiguous recent run: approximately 30 minutes for a
  short window, two hours for weekly-or-longer windows. At least three points
  covering two thirds of that horizon are required. The live observation replaces
  its mutable ten-minute bucket, never counting as another elapsed interval.
  Confirmed acceleration switches the weekly smoother to the fresh 30-minute
  horizon until the warning clears, so normal smoothing cannot conceal it.
  A fresh snapshot is not evidence of fresh consumption: unchanged updates do
  not renew the activity deadline. The coarse estimate expires at the end of the
  last bucket with an observed increase plus five minutes (short) or 30 minutes
  (weekly), capped by reset. A bucket-end bound is not an exact activity time.
  Continuous session-local observations can establish unchanged quota sooner
  and override the coarse estimate; gaps or reset changes invalidate that proof.
- **Usual pace** uses fully observed, non-overlapping blocks of the same horizon,
  excluding the most recent 24 hours. Rates are time-weighted within each quota
  reported reset-deadline group; the median group rate is the reference. This retains observed idle time
  and avoids mistaking regular bursts separated by idle time for zero usage.
  For an even number of groups, the upper middle rate is used as a conservative
  depletion reference; sparse history is not a calibrated confidence statement.
  At least 12 blocks, two reported reset deadlines and three observed UTC dates
  are required. The legacy wire field `historical_cycles` counts distinct
  reported reset timestamps, not verified independent quota cycles; the UI says
  `reported reset times` rather than presenting that count as confidence.

Both estimates start from the currently reported remaining percentage. A
depletion timestamp is returned only when depletion would precede the reported
reset; otherwise the UI says `At this pace: reset first` or `Historical pace:
reset first`. A depletion timestamp is anchored to the quota observation, not
the later Doctor/display time: rereading the same observation cannot postpone
its prediction. Durations are rounded rather than presented as minute-exact promises.
Insufficient recent history falls back to the existing session-local estimate;
insufficient ordinary history does not invent a usual pace or warning. Older
routers continue to use the session-local estimate.

Missing buckets, unknown durations/reset times, percentage decreases, duration
changes, resets and saturated (100%-used) intervals break continuity. Missing or
exhausted time is never counted as idle. Different accounts and window kinds are
never pooled. All displayed forecasts must match the exact current quota's kind,
duration, reset, percentage and observation time; observations aged 60 seconds,
elapsed resets and failed refreshes suppress them.

The header/event/live-usage parsers preserve upstream absolute `reset_at`
values; the router does not round or re-anchor them. The reviewed native parser
also reads the absolute integer directly. Small timestamp differences therefore
cannot be attributed to a local rounding rule. Retained history does not record
which upstream metadata source supplied a sample, so it cannot establish the
cause of upstream jitter or prove that nearby deadlines identify the same cycle.
Forecast continuity remains **exact**, even for a one-second change. Reported
quota, storage, routing and credit-cycle identities are never normalized.

Optional `current_status` and `current_valid_until` explain the current estimate:
`recent_pace`, `insufficient_history`, `quota_unchanged`, `reset_changed` or
`reset_time_moving`. The moving-reset description requires consecutive zero-used
observations whose deadlines move with observation time while their remaining
duration stays near the full reported window; it does not prove an unstarted
window or merge cycles. The UI distinguishes building history, a changed/moving
reset, and unchanged quota, rather than inventing a depletion time. Unchanged
percentage does not prove zero token usage or an idle application.

The current label explicitly says `At this pace`; the separate historical label
says `Historical pace`. Neither forecasts future workload or billing expiry.
An activity-expired estimate and warning stay hidden even if a local fallback
still retains an older trend. An already elapsed conditional depletion deadline
is shown as needing fresh quota, never reissued as a future countdown. Historical
pace remains separate when recent pace is unavailable. Older metadata without
the optional status/deadline fields remains readable.

### Weekly calendar time and short-window pauses

The weekly window's raw `current` projection remains inspectable and independent
of the short window. It is **not** by itself a wall-clock depletion forecast.
The optional weekly `calendar` object supplies a separate current projection,
the exact companion `short` observation, `valid_until` and a bounded `pauses`
count. Updated clients verify **both** observations, not just the weekly one.
Unknown calendar metadata is not inferred from the chart or another account.

For an account reporting both windows, a calendar estimate requires fresh,
same-time observations and a contiguous, paired run of both windows with exact
reset and duration continuity. Both rates use the same endpoints and the weekly
recent horizon/evidence requirements. Any missing, exhausted, decreased,
reset-changed or unaligned short-window interval breaks that run. In particular,
a weekly rate averaged over an observed short-window block is not slowed again
by adding that same block. The short rate must be positive and recently active;
calendar validity is bounded by both activity deadlines and the reported short
reset. Different primary/secondary order and actual short durations are supported.

The conditional scenario spends both percentages at those paired rates until
the first short exhaustion or reported reset. If the short budget expires first,
weekly consumption pauses until that reset. The scenario then repeats full
short-window capacity and its reported duration, resuming immediately each time.
Subsequent modeled reset times are **assumptions**, not upstream observations;
they are never persisted as quota, used for routing or used for reset credits.
The calculation is closed-form rather than an unbounded loop over future windows.
Depletion at or after the weekly reset is shown as reset-first, not exhaustion.

The UI adds `incl. short-window pauses` when the scenario includes such pauses.
A currently exhausted short window shows `Weekly pace paused: short window
exhausted`, rather than inventing a rate after recovery. If paired evidence is
unavailable, including immediately after a short reset, the weekly raw or legacy
session-local fallback is explicitly labeled `Without future pauses`; it is not
presented as a pause-adjusted calendar ETA. A weekly-only snapshot needs no short
adjustment. Older routers without `calendar` remain readable, but their weekly
estimates get the same unadjusted label in updated clients. Updated server and
clients are required for calendar-aware display; this is not a release claim.

Historical `usual` projections remain separate and unchanged: their observed
rates may already include idle or blocked time. Do not add a second pause schedule
to those rates. An acceleration warning can still describe unusually high
consumption, but its weekly before-reset phrase requires a valid calendar
projection predicting depletion before reset; the raw warning flag retains its
single-window meaning for compatibility. The scenario does not predict future
workload, changes in model mix, routing, provider accounting, extra reset credits
or whether users will resume work immediately. It is not calibrated accuracy or
a guarantee that the upstream will enforce the same future windows.

`Usage above usual` requires a recent rate above both 1.5 times the usual pace
and 1.1 times the historical 30-minute block-rate 90th percentile, with at least one
percentage point consumed over the recent run. Three consecutive **closed**
bucket observations must each show new consumption faster than 1.5 times usual.
Detection uses the recent 30-minute run for both short and weekly windows.
Two observations with no new consumption, or with a recent rate below the larger
of 1.2 times usual and the historical 75th percentile, clear the warning. Closed
buckets are replayed after restart, so repeated dashboard refreshes cannot
confirm an alert. A separate warning phrase identifies projected depletion
before reset. Zero ordinary usage gives an unknown ratio, not infinity.

These are conditional estimates, not statistical confidence intervals. Thresholds
have offline synthetic walk-forward, burst, idle, gap, reset, freshness and
restart coverage; accuracy and false-positive rates on real workloads have not
been calibrated. Day-of-week seasonality and future workload/routing changes are
not modeled. A change in workload, plan or provider accounting can invalidate
historical expectations; the estimate does not diagnose its cause.

### Private offline evaluation

The offline tools replay the **actual Rust estimator**, not a Python copy of its
algorithm. Collection uses only passive Doctor and QuotaHistory management reads
through the existing saved `exr` profile. It never invokes Limits, refreshes
upstream, submits inference or redeems a credit. Only allowlisted numeric quota
points and run-local `account-NNN` aliases are exported; account IDs, email
labels, credentials, clients and Platform data are discarded. Aliases need not
identify the same account across separate exports if the pool changed.

Keep exports and reports outside the checkout. Choose a new private directory;
output files are created exclusively with mode 0600, never overwritten:

```sh
WORK=/path/to/private/analysis
mkdir -m 700 "$WORK"
python3 scripts/collect-quota-history.py --exr target/debug/exr --output "$WORK/history.json"
cargo run --locked --example quota-forecast-replay -- --input "$WORK/history.json" --output "$WORK/replay.jsonl"
python3 scripts/evaluate-quota-forecast.py --replay "$WORK/replay.jsonl" --output "$WORK/report.json"
```

Replay excludes mutable current buckets and supplies only observations available
at each simulated time, retaining at most the estimator's 30-day lookback.
Companion windows at the same timestamp become available together; later
companion observations are never borrowed. `runtime_current` retains the raw
single-window comparison, while `runtime_calendar` separately scores calendar
depletion ETAs and availability. Its paired active rate is not treated as a
linear wall-clock consumption predictor at 10/30/60/120-minute horizons.
Evaluation reserves the last third of **observed UTC buckets** as validation;
it is not a split by two thirds of elapsed calendar time. Validation forecasts
may use earlier training history, but training outcome labels stop before the
boundary. Missing hourly-era observations are never backfilled into ten-minute
samples. The collector can select accounts with repeated `--account ID` options
and reads at most 90 days per account, without paging a running service.

The report separates short/weekly windows and training/validation periods. It
includes forecast availability, percentage-point error at 10/30/60/120-minute
horizons, depletion-time error, deadline jumps and observed warning counts.
Current-versus-proxy comparisons also use identical paired observations, so
coverage differences cannot masquerade as improved precision. Per-event macro
errors and actual event counts expose repeated correlated predictions of one
depletion. An event is recognized only when contiguous observations bracket a
transition from below 100% to exhaustion before reset; resets, disappearance and
already-exhausted initial samples are not successful depletion labels. ETA error
is distance to that observation interval, not to an invented exact timestamp.

The interval-linear comparator is explicitly a **downsampled proxy**, not the old
session-local predictor: ten-minute history cannot reconstruct its sub-minute
freshness and idle checks. Scores cover the runtime estimator, not the complete
displayed forecast when a client falls back to that unavailable fine-grained
session history. Outcome grouping permits up to 60 seconds of reset
timestamp jitter around a fixed episode anchor, independently of any estimator
candidate. Large changes, percentage decreases, duration changes and missing
buckets still split episodes. This labeling tolerance does not change runtime
reset handling or authorize routing/account/reset-credit actions.

Interpretation limits:

- Better near-term consumption error does not establish a better depletion ETA.
- Zero warnings with no supported usual-pace estimates do not prove a low
  false-positive rate. Warning continuation is not an objective false-alarm label.
- Observations and overlapping forecast horizons are correlated. A single
  depletion event is not many independent validation examples.
- Sparse ten-minute history, timestamp jitter and floating unused-window resets
  are reported as data-quality limitations, not silently repaired.
- Tune on training data only and validate on later independent cycles. Do not
  lower evidence requirements just to make forecasts or warnings appear.
- Reusing an already reviewed export is a regression comparison, not a new
  independent holdout. Report estimate availability as well as paired errors;
  hiding an unsupported idle estimate is not proof of improved ETA accuracy.

Synthetic tool checks:

```sh
python3 scripts/test-quota-forecast-evaluation.py
cargo test --locked --example quota-forecast-replay
```

## Schema 15 to 16: ten-minute buckets

Migration 016 transactionally rebuilds only the numeric history table with
600-second alignment and observation constraints. Each old hourly sample moves
to the bucket containing its actual `observed_at`. No samples are invented for
the other five buckets; old gaps remain gaps. Credentials, keys, tokens, usage,
current quotas, routing and reset ledgers are unchanged. Retention remains 365
days, with a sixfold maximum row count. Allow disk space for a temporary second
copy of the history table.

This is not a routine one-click deployment. With operator authorization, stop
the runtime, create and verify a private consistent backup of current state and
keys, and rehearse the migration on a private copy. Deploy matching server and
gateway binaries, verify schema 16, integrity/foreign keys, health, authentication
and retained numeric history without inference or credit consumption. Update
native clients and the Desktop plugin together; old Desktop plugins reject the
new interval. Publishing source does not change deployed state.
Desktop subprocess/stdio reports remain bounded to 8 MiB so a full 90-day
ten-minute history fits; concurrent read limits are unchanged.

For rollback, stop the runtime and preserve a verified **current** state/key
backup first. Do not restore a pre-upgrade database or simply lower user_version:
schema 15 cannot write ten-minute buckets. On a private copy, rehearse rebuilding
the table using migration 014's original constraints: retain the latest
`observed_at` (then `request_order` on ties) per account/kind/UTC hour, rounding
the bucket start down to 3,600 seconds. Preserve an offline private copy of all
fine-resolution rows before reducing them. Recreate the original pruning index,
set `PRAGMA user_version=15` in the same transaction, and verify integrity,
foreign keys and unchanged non-history state. Restore matching old server and
gateway binaries and check read-only health/authentication. Re-upgrade remaps
the retained hourly samples; it does not recover discarded fine-resolution rows.

## Original schema 13 to 14: reviewed migration and rollback

Migration 014 adds only `oauth_quota_history` and an hourly pruning index.
Existing credentials/generations, policies, limits, cooldown/order fences,
usage, reset/activation ledgers, search affinity and keys are untouched. Each
accepted quota write stores its history in the same transaction and under the
same generation/ordering/absence fences. Rejected or late observations cannot
overwrite history. At most 8,760 rows per account/kind survive a write, even after
a backward clock adjustment. Old hours are pruned at startup and the next quota
write; stale accounts' retained rows stay bounded and are excluded from reports.

Publishing this release does **not** migrate a deployed server. Routine Docker
updates must continue to refuse a schema mismatch. With operator authorization:

1. Inspect the actual deployment and preserve its units, Compose overrides,
   ingress, SSH boundaries and private configuration. Stop the old runtime and
   retain its exact server/gateway binaries or pinned image.
2. Use the old version to create and verify a consistent private state/keys
   snapshot. Rehearse restore into a private scratch directory, never an active
   second router. Preserve the snapshot and both existing keys.
3. Deploy matching new server/gateway/standalone binaries and allow the normal
   transactional startup migration. Verify schema 14, SQLite integrity/foreign
   keys, health and authenticated read-only diagnostics. Confirm retained
   policies/authentication and numeric history without sending inference or
   spending credits. Update native clients to retain the new Doctor field.

For rollback, stop the new runtime and back up **current** state/keys first.
Verify that the database is exactly schema 14 with only the reviewed migration
014 additions. In a reviewed offline transaction, retain the additive table/index
and their rows and set `PRAGMA user_version=13`; restore the old server/gateway
binaries together. Old code ignores the table and stops collecting history.
Do not restore the pre-upgrade database just to roll back code: doing so can
discard usage or rotated credentials. Run integrity/foreign-key and read-only
health/authentication checks. Re-upgrade is idempotent and retains in-range
history. No migration/rollback deploy is performed by the GUI or restricted SSH
gateway.
