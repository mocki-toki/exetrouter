# Capacity outlook

OpenCode Desktop Overview has a compact, English **Capacity outlook** card:
estimated runway, a possible interruption, expiring reset credits, and an
estimated load reduction when one can bridge the next recovery. **Details**
shows the selected model, per-account short/weekly reserves and reset times,
availability/recovery, recent request trend, confidence and separate scenarios.
The forecast covers the next **24 hours**, not an unlimited runway or SLA.

## Scope and calibration

The selected model is always visible. Details can change it; the default is
`gpt-6.1-sol` when reported, otherwise the first catalog model. Only active,
operator-enabled accounts supporting that model can supply capacity. Operational
backoff, non-subscription cooldown, unavailable credentials/catalogs and disabled
accounts are excluded; a quota reset does not repair those conditions. Model-only
limits not reported in the general subscription meters remain outside this
estimate. Native image quotas are separate and are not forecast in this view.
The entire recent text-request rate is projected onto the selected model's pool,
conservatively assuming it must carry the whole current text load.

Doctor adds optional, typed `capacity` numeric metadata and
`quota_accounts[].capacity` profiles. These are passive management diagnostics,
not a public HTTP endpoint. Older routers/native clients/IPC reports show
**Insufficient data** instead of inventing capacity or failing the dashboard.
No inference, upstream metadata refresh, credit redemption or new persistent
state is introduced by reading the forecast. Standalone shares the same code.

Each account/window is calibrated from its own latest contiguous hourly quota
observations plus the current sample. Calibration needs at least 15 minutes,
five text requests and one percentage point of observed change in the same quota
cycle; observations cannot have a gap over 90 minutes or span more than six hours.
Resets, duration changes and decreasing usage break a series. A calibration
interval containing weekly activation, or over 10% unknown usage, is unavailable.
The model uses percentage points **per recent average request on that account**,
not a sum of percentages across plans, API dollars, or token-derived quota.
Requests include all users but expose no token/user identities or content.

Fresh observations, confirmed short/weekly window structure, future reset
deadlines and per-window calibration are required for every eligible account.
Unknown/cold reserves are neither zero nor infinite. Too little recent traffic
does not imply unlimited capacity. Already exhausted pools can show their
observed blocking reset times even when no burn-rate forecast is possible.

## Simulation and uncertainty

The bounded desktop simulation uses minute steps, at most 64 accounts and a
24-hour horizon. It redistributes recent average requests on exhaustion using
urgent deadlines, then short-window accounts before weekly-only reserves and
approximate weekly pacing weights. Each request consumes both applicable
windows according to that account's calibration. An account returns only when
all blocking windows can recover; a short reset cannot replenish its week.
Following a known natural reset, the next modeled cycle starts when work resumes.

The base scenario keeps the last-hour request rate. The higher-load scenario
uses 125% of the greater of the last and previous hours. Suggested reduction is
a bounded search for a rate that bridges the first useful recovery within the
horizon, not a request-frequency limit or an automatic throttle. It never changes
routing, account affinity or automatic reset policy.

**Medium** confidence requires a stable recent request count and no observed
in-flight work; otherwise confidence is **Low**. No high-confidence guarantee is
made: request sizes, model mix, Fast-mode changes, external use of the same OAuth
accounts, upstream policy and pinned/unrecoverable contexts can diverge from
this model. Stale/disconnected snapshots suppress numerical planning estimates.

## Credits are conditional, never confirmed supply

The compact card reports only fresh, supported credits expiring within 72 hours.
Incomplete inventory is not extrapolated from its total count. The default
forecast never adds a credit to available quota.

A separate scenario can model **one currently eligible credit per account**,
after the normal recheck delay, only if its expiry leaves time for that delay.
A read-only preview shares the actual reset eligibility rules and checks the
worker-observed inventory/order, cycle/credit ledger, pending/unknown recovery,
new-consumption and final-day cohort fences. The view never observes, claims,
reserves or retries a credit. Strategic credits and hypothetical future
eligibility are not assumed. The scenario assumes successful reset and observed
recovery; real spending remains solely with the existing automatic worker.
