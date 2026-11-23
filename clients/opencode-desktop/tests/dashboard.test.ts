import assert from "node:assert/strict"
import { mkdtemp, writeFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { Schema } from "effect"
import { best, Forecasts, priority, routingFresh, visibleWindows, historicalForecast } from "../src/dashboard"
import { activity, activityDate, activityIndex, activityValue, dollars } from "../src/chart"
import { NativeClient, snapshot, models, tokens, usage } from "../src/native"
import { options } from "../src/options"
import { Models, Profile, Tokens, Usage, Snapshot } from "../src/schemas"
import { deferred, doctor, forecastReport, weeklyCalendarReport, modelList, tokenList, usage as usageFixture } from "./fixtures"
import { requestAge } from "../src/format"

test("client request ages use seconds, minutes, hours and days without inventing unknown times", () => {
  const now = 1_800_000_000
  for (const [age, expected] of [[0, "0s ago"], [59, "59s ago"], [60, "1m ago"], [3599, "59m ago"], [3600, "1h ago"], [1286 * 60, "21h ago"], [86400, "1d ago"], [172800, "2d ago"]] as const) {
    assert.equal(requestAge(now - age, now), expected)
  }
  assert.equal(requestAge(now - .5, now), "0s ago")
  for (const at of [null, now + 1, NaN, Infinity]) assert.equal(requestAge(at, now), "Unknown")
  assert.equal(requestAge(now, NaN), "Unknown")
})

test("client labels from older native reports never expose private ciphertext", () => {
  const raw = doctor()
  raw.quota_accounts[0].clients.rows[0].name = "exrp1:synthetic-ciphertext"
  const report = snapshot(raw)
  assert.equal(report.accounts[0].clients.rows[0].name, "Label unavailable")
  assert.doesNotMatch(JSON.stringify(report), /exrp1:|synthetic-ciphertext/)
})

test("chart indexes duplicate buckets, selects by identity and excludes future activity", () => {
  const raw = usageFixture()
  const first = { ...raw.rows[0], name: "first", display_name: "Same label", input_tokens: 100, output_tokens: 50 }
  const second = { ...first, name: "second", input_tokens: 200 }
  const report = usage({ ...raw, rows: [first, second], timeline: [
    { from_utc: 1800000000, to_utc: 1800003600, rows: [first, second, first] },
    { from_utc: 1800003600, to_utc: 1800007200, rows: [second] },
  ] }, "token")
  const data = activity(report, "first", "tokens", 1800000000)
  assert.equal(data.series.length, 1)
  assert.deepEqual(data.series[0].values, [300])
  assert.equal(data.maximum, 500)
  assert.equal(data.buckets.length, 1)
  assert.equal(data.reported, 150)
  assert.equal(activity(report, "", "requests", 1800007200).series.length, 2)
  assert.doesNotMatch(data.series[0].points, /NaN|Infinity/)
  assert.equal(activity(report, "missing", "tokens", 1800000000).rows.length, 0)
})

test("chart cursor maps scaled pointer positions to real buckets and preserves unknown counters", () => {
  assert.equal(activityIndex(0, 100, 0, 668), undefined)
  assert.equal(activityIndex(4, 100, 0, 0), undefined)
  assert.equal(activityIndex(4, 10 + 44 / 2, 10, 334), 0)
  assert.equal(activityIndex(4, 10 + 644 / 2, 10, 334), 3)
  assert.equal(activityIndex(5, 344, 0, 668), 2)
  assert.equal(activityIndex(5, -1000, 0, 668), 0)
  assert.equal(activityIndex(5, 1000, 0, 668), 4)
  const bucket = usage(usageFixture(), "model").timeline[0]
  assert.deepEqual(activityValue(bucket, "test-reasoner", "tokens"), { value: 0, incomplete: true })
  assert.deepEqual(activityValue({ ...bucket, rows: bucket.rows.map((r) => ({...r, input:null, output:null})) }, "test-reasoner", "tokens"), { value:null, incomplete:true })
  assert.deepEqual(activityValue(bucket, "test-reasoner", "requests"), { value:2, incomplete:false })
})

test("USD chart preserves model prices, partial totals and small dollar scales", () => {
  const raw = usageFixture(), first = { ...raw.rows[0], name: "gpt-6-astra", requests: 2, estimated_usd: 10, unpriced_requests: 1 }
  const second = { ...first, name: "gpt-6.1-sol", requests: 1, estimated_usd: 0.00004, unpriced_requests: 0 }
  const report = usage({ ...raw, rows: [first, second], timeline: [{ from_utc: 1800000000, to_utc: 1800003600, rows: [first, second] }] }, "model")
  const data = activity(report, "", "usd", 1800003600)
  assert.equal(data.usd, 10.00004)
  assert.equal(data.unpriced, 1)
  assert.deepEqual(activityValue(data.buckets[0], "gpt-6-astra", "usd"), { value: 10, incomplete: true })
  assert.ok(Math.abs(activity(report, "gpt-6.1-sol", "usd", 1800003600).maximum - 0.00005) < 1e-12)
  assert.equal(dollars(0.00004), "$0.00004")
  assert.equal(dollars(undefined), "—")
  assert.equal(dollars(-1), "-$1")
  assert.equal(dollars(-5000), "-$5k")
  for (const [value, formatted] of [[5000,"$5k"],[12500,"$12.5k"],[1200000,"$1.2M"],[999999,"$1M"],[100,"$100"],[0,"$0"]] as const) assert.equal(dollars(value),formatted)
  const unknown = usage({ ...raw, rows: [{ ...first, estimated_usd: null }], timeline: [{ from_utc: 1800000000, to_utc: 1800003600, rows: [{ ...first, estimated_usd: null }] }] }, "model")
  assert.equal(activity(unknown, "", "usd", 1800003600).usd, null)
  assert.equal(activityValue(unknown.timeline[0], "gpt-6-astra", "usd").value, null)
})

test("previous IPC usage rows without USD fields render as unknown, not free", () => {
  const report = usage(usageFixture(), "model")
  const legacyRow = ({ usd: _usd, unpriced: _unpriced, ...row }: Usage["rows"][number]) => row
  const legacy: Usage = { ...report, rows: report.rows.map(legacyRow), timeline: report.timeline.map((bucket) => ({ ...bucket, rows: bucket.rows.map(legacyRow) })) }
  Schema.decodeUnknownSync(Usage)(legacy)
  const data = activity(legacy, "", "usd", Number.MAX_SAFE_INTEGER)
  assert.equal(data.usd, null)
  assert.equal(data.unpriced, report.rows.reduce((sum, row) => sum + row.requests, 0))
  assert.equal(data.series[0].values[0], null)
  assert.doesNotMatch(data.series[0].points, /NaN|Infinity/)
  assert.deepEqual(activityValue(legacy.timeline[0], report.rows[0].key, "usd"), { value: null, incomplete: true })
  assert.equal(dollars(legacy.rows[0].usd), "—")
})

test("chart hover labels use one hourly endpoint or the daily bucket's date", () => {
  const from = new Date(2026, 9, 28, 0, 0).getTime() / 1000
  const bucket = { from, to: from + 3600, rows: [] }
  assert.equal(activityDate(bucket, "24h"), "Oct 28, 01:00 AM")
  assert.equal(activityDate(bucket, "day"), "Oct 28, 01:00 AM")
  assert.equal(activityDate({ ...bucket, to: from + 86400 }, "week"), "Oct 28")
  assert.equal(activityDate(bucket, "month"), "Oct 28")
})

test("projects every TUI overview/detail field without native error payloads", () => {
  const raw = doctor()
  const report = snapshot(raw), a = report.accounts[0]
  assert.equal(report.activeAccounts, 3)
  assert.equal(report.reauthAccounts, 1)
  assert.equal(report.models, 7)
  assert.deepEqual(report.generations, { active: 2, limit: 8 })
  assert.equal(a.clients.rows[0].name, "Synthetic client")
  assert.equal(a.clients.rows[0].running, 2)
  assert.equal(a.credits.count, 2)
  assert.equal(a.routing.targetShare, 65)
  assert.equal(best(a, report.checkedAt), true)
  assert.equal(best(a, report.checkedAt + 60), false)
  assert.equal(best({ ...a, enabled: false }, report.checkedAt), false)
  assert.equal(priority({ ...a, enabled: false }), false)
  assert.equal(routingFresh({ ...a, routing: { ...a.routing, validUntil: report.checkedAt } }, report.checkedAt), false)
  assert.equal(snapshot({ ...raw, quota_accounts: [{ ...raw.quota_accounts[0], refresh_error: "private-body-marker", secret: "private-body-marker" }] }).accounts[0].refreshError, true)
  assert.doesNotMatch(JSON.stringify(snapshot({ ...raw, quota_accounts: [{ ...raw.quota_accounts[0], refresh_error: "private-body-marker" }] })), /private-body-marker/)
  assert.equal(visibleWindows({ ...a, windows: [{ ...a.windows[0], minutes: null }] }).length, 0)
  const unknown = snapshot({ ...raw, quota_accounts: [{ ...raw.quota_accounts[0], quota: { windows: [{ ...raw.quota_accounts[0].quota.windows[0], remaining_percent: null, used_percent: null }] } }] })
  assert.equal(unknown.accounts[0].windows[0].remaining, null)
})

test("one paused account must not be reported as a pool-wide subscription cooldown", () => {
  const raw=doctor()
  const partial={...raw,configuration_status:"needs_attention",catalog:{...raw.catalog,status:"stale"},pool:{cooldown_accounts:1,backoff_accounts:0}}
  assert.equal(snapshot(partial).routingStatus,"Catalog refresh on next request")
  assert.equal(snapshot({...partial,pool:{cooldown_accounts:3}}).routingStatus,"All accounts are paused")
})

test("keeps usage timeline identities, cached counters and unknowns for all TUI groups", () => {
  const raw = usageFixture()
  const report = usage(raw, "total")
  Schema.decodeUnknownSync(Usage)(report)
  assert.equal(report.timeline[0].rows[0].output, null)
  assert.equal(report.timeline[0].rows[0].key, "test-reasoner")
  assert.equal(report.rows[0].cached, null)
  const tokens = usage({ ...raw, rows: [
    { ...raw.rows[0], name: "first", display_name: "Same label", cached_input_tokens: 123 },
    { ...raw.rows[0], name: "second", display_name: "Same label" },
  ] }, "token")
  assert.notEqual(tokens.rows[0].key, tokens.rows[1].key)
  assert.equal(tokens.rows[0].name, tokens.rows[1].name)
  assert.equal(tokens.rows[0].cached, 123)
})

test("projects token metadata and reverses models just like the TUI", () => {
  const raw = tokenList()
  Schema.decodeUnknownSync(Tokens)(tokens(raw))
  assert.equal(tokens(raw)[0].lastUsedAt, null)
  assert.equal(tokens([{ ...raw[0], revoked_at: 1800000001 }])[0].status, "Revoked")
  assert.equal(tokens([{ ...raw[0], expires_at: 1 }])[0].status, "Expired")
  const result = models({ data: [...modelList().data, { id: "last", secret: "private-body-marker" }] })
  Schema.decodeUnknownSync(Models)(result)
  assert.equal(result[0].id, "last")
  assert.equal(result[0].context, null)
  assert.doesNotMatch(JSON.stringify(result), /private-body-marker/)
})

test("all additional bridge commands are fixed passive reads; total omits --by", async () => {
  const calls: string[][] = []
  const client = new NativeClient(options({}), async (_, args) => {
    calls.push([...args])
    if (args[0] === "models") return modelList()
    if (args[0] === "token") return tokenList()
    return usageFixture()
  })
  const signal = new AbortController().signal
  await client.tokens(signal); await client.models(signal); await client.usage({ by: "total", period: "24h" }, signal)
  assert.deepEqual(calls, [["token", "list", "--json"], ["models", "--json"], ["usage", "--period", "24h", "--json"]])
})

test("settings reads allowlisted profile metadata only and does not expose credentials", async () => {
  const root = await mkdtemp(join(tmpdir(), "exr-profile-test-"))
  try {
    const path = join(root, "profile.json")
    await writeFile(path, JSON.stringify({ mode: "remote", host: "router.example.com", port: 2222, ssh_user: "routercli", identity: "/synthetic/key", secret: "private-body-marker" }))
    const client = new NativeClient(options({ exrConfig: path }), async () => { throw new Error("Remote settings must not launch subprocesses.") })
    const report = await client.profile(new AbortController().signal)
    Schema.decodeUnknownSync(Profile)(report)
    assert.equal(report.destination, "routercli@router.example.com:2222")
    assert.doesNotMatch(JSON.stringify(report), /private-body-marker/)
    await writeFile(path, "x".repeat(65537))
    await assert.rejects(client.profile(new AbortController().signal))
  } finally { await rm(root, { recursive: true, force: true }) }
})

test("standalone settings lists only account identity/state and never starts OAuth", async () => {
  const root = await mkdtemp(join(tmpdir(), "exr-local-profile-test-"))
  try {
    const path = join(root, "profile.json")
    await writeFile(path, JSON.stringify({ mode: "standalone", listen: "127.0.0.1:8787", state_dir: "/synthetic/state" }))
    const gate = deferred<unknown>()
    const client = new NativeClient(options({ exrConfig: path }), async (_, args) => {
      if (args[2] === "doctor") return gate.promise
      assert.deepEqual(args, ["--config", path, "account", "list", "--json"])
      return [{ id: 1, email: "account@example.com", state: "active", oauth: "private-body-marker" }]
    })
    const signal = new AbortController().signal
    const pendingSnapshot = client.snapshot(signal)
    const report = await client.profile(signal)
    gate.resolve(doctor())
    await pendingSnapshot
    assert.equal(report.mode, "Standalone")
    assert.deepEqual(report.accounts, [{ id: 1, email: "account@example.com", state: "active" }])
    assert.doesNotMatch(JSON.stringify(report), /private-body-marker/)
    const abort = new AbortController(); abort.abort()
    await assert.rejects(client.profile(abort.signal), /cancelled/)
  } finally { await rm(root, { recursive: true, force: true }) }
})

test("release check is explicit, respects EXR_NO_UPDATE_CHECK and never installs", async () => {
  const calls: string[][] = []
  const client = new NativeClient(options({}), async (_, args) => {
    calls.push([...args])
    return { current: "0.0.0", latest: "v0.0.1", available: true, secret: "private-body-marker" }
  })
  const previous = process.env.EXR_NO_UPDATE_CHECK
  try {
    process.env.EXR_NO_UPDATE_CHECK = "1"
    await assert.rejects(client.update(new AbortController().signal), /disabled/)
    assert.equal(calls.length, 0)
    delete process.env.EXR_NO_UPDATE_CHECK
    assert.deepEqual(await client.update(new AbortController().signal), { current: "0.0.0", latest: "v0.0.1", available: true })
    assert.deepEqual(calls, [["update", "--check", "--json"]])
  } finally {
    if (previous === undefined) delete process.env.EXR_NO_UPDATE_CHECK
    else process.env.EXR_NO_UPDATE_CHECK = previous
  }
})

test("quota forecasts match native elapsed-span, idle, reset and freshness fences", () => {
  const forecasts = new Forecasts(), base = 1800000000
  const report = (at: number, used: number) => {
    const r = snapshot(doctor()), a = r.accounts[0], w = a.windows[0]
    return { ...r, checkedAt: at, accounts: [{ ...a, windows: [{ ...w, observedAt: at, used, remaining: 100 - used, resetAt: base + 604800 }] }] }
  }
  forecasts.observe(report(base, 10), base)
  const active = report(base + 60, 12)
  const [a] = active.accounts, w = a.windows[0], eta = forecasts.observe(active)
  assert.equal(eta(a, w, base + 60), "-44m")
  assert.equal(eta({...a, id:2}, w, base + 60), undefined)
  assert.equal(eta({...a, observationKey:"b".repeat(64)}, w, base + 60), undefined)
  assert.equal(eta(a, w, base + 120), "-44m", "the display clock does not recompute a cached ETA")
  assert.equal(eta(a, w, base + 45 * 60), undefined)
  for (let elapsed = 30; elapsed <= 300; elapsed += 30) {
    const r = report(base + 60 + elapsed, 12)
    assert.equal(!!forecasts.observe(r)(r.accounts[0], r.accounts[0].windows[0], r.checkedAt), elapsed < 300)
  }
  const reset = report(base + 390, 0)
  assert.equal(forecasts.observe(reset)(reset.accounts[0], reset.accounts[0].windows[0], reset.checkedAt), undefined)
})

test("shared history forecasts survive IPC and are matched to the exact current quota", () => {
  const raw = forecastReport(), report = snapshot(raw), account = report.accounts[0], w = account.windows[0], now = report.checkedAt
  Schema.decodeUnknownSync(Snapshot)(report)
  const f = historicalForecast(account, w, now)
  assert.ok(f)
  const forecast = (value: typeof f) => new Forecasts().observe({ ...report, accounts: [{ ...account, forecasts: [value] }] })(account, w, now)
  assert.equal(forecast(f), "-3h 0m")
  assert.equal(historicalForecast(account, w, now + 60), undefined)
  assert.equal(historicalForecast({ ...account, refreshError: true }, w, now), undefined)
  for (const changed of [{ ...w, used: 51 }, { ...w, resetAt: w.resetAt! + 1 }, { ...w, observedAt: now - 1 }, { ...w, minutes: 300 }, { ...w, kind: "secondary" }, { ...w, status: "stale" }]) {
    assert.equal(historicalForecast(account, changed, now), undefined)
  }
  assert.equal(forecast({ ...f, current: { percentPerHour: 1, limitAt: null } }), undefined)
  assert.equal(forecast({ ...f, usual: { percentPerHour: 0, limitAt: null } }), "-2h 0m")
  assert.equal(forecast({ ...f, current: null }), undefined, "history alone never produces an ETA")
  assert.equal(historicalForecast(snapshot(doctor()).accounts[0], w, now), undefined)
})

test("history forecast projection rejects malformed numeric evidence without forwarding payloads", () => {
  const raw = forecastReport(), f = raw.quota_accounts[0].quota_forecast[0]
  const withForecast = (forecast: unknown) => ({ ...raw, quota_accounts: [{ ...raw.quota_accounts[0], quota_forecast: [forecast] }] })
  for (const invalid of [
    { ...f, kind: "private-body-marker" }, { ...f, window_minutes: 0 }, { ...f, used_percent: 100 },
    { ...f, current: { percent_per_hour: -1, limit_at: null } },
    { ...f, current: { percent_per_hour: 25, limit_at: f.reset_at } },
    { ...f, current: null }, { ...f, historical_cycles: 0 },
    { ...f, warning: { rate_ratio: Infinity, limit_before_reset: true } },
    { ...f, warning: { rate_ratio: 2, limit_before_reset: false } },
    { ...f, current_status: "private-body-marker" }, { ...f, current_status: "quota_unchanged" },
    { ...f, current_valid_until: f.observed_at }, { ...f, current_valid_until: f.reset_at + 1 },
  ]) assert.throws(() => snapshot(withForecast(invalid)))
  const safe = snapshot(withForecast({ ...f, secret: "private-body-marker" }))
  assert.doesNotMatch(JSON.stringify(safe), /private-body-marker/)
  assert.throws(() => snapshot({ ...raw, quota_accounts: [{ ...raw.quota_accounts[0], quota_forecast: [f, f] }] }))
})

test("a new snapshot drops expired current pace without showing historical-only estimates", () => {
  const raw = forecastReport(), base = raw.checked_at
  const f = raw.quota_accounts[0].quota_forecast[0]
  const withStatus = { ...raw, quota_accounts: [{ ...raw.quota_accounts[0], quota_forecast: [{ ...f, current_status: "recent_pace", current_valid_until: base + 20 }] }] }
  const r = snapshot(withStatus), a = r.accounts[0], w = a.windows[0], model = historicalForecast(a,w,base)!
  Schema.decodeUnknownSync(Snapshot)(r)
  const cache = new Forecasts(), estimate = cache.observe(r, base+19)
  assert.equal(estimate(a,w,base+19), "-3h 0m")
  assert.equal(estimate(a,w,base+20), "-3h 0m")
  assert.equal(cache.observe(r,base+20)(a,w,base+20), undefined)
  const passed = { ...model, currentValidUntil: base+40, current: { percentPerHour:100, limitAt:base+5 } }
  assert.equal(cache.observe({...r,accounts:[{...a,forecasts:[passed]}]},base+5)(a,w,base+5), undefined)
  for (const currentStatus of ["reset_changed", "reset_time_moving", "insufficient_history", "quota_unchanged"] as const) {
    const update = {...r,accounts:[{...a,forecasts:[{...model,current:null,currentValidUntil:undefined,currentStatus}]}]}
    assert.equal(cache.observe(update)(a,w,base),undefined)
  }
})

test("continuous fine-grained idle observations override a coarse runtime estimate", () => {
  for (const minutes of [300,10080]) {
    const initial = snapshot(forecastReport()), base = initial.checkedAt, template = initial.accounts[0]
    const fallback = new Forecasts()
    const report = (at: number, used: number) => ({ ...initial, accounts: [{ ...template, windows: [{ ...template.windows[0], minutes, observedAt: at, used, remaining:100-used }], forecasts: [{ ...template.forecasts![0], minutes, observedAt:at, used, currentStatus:"recent_pace" as const, currentValidUntil:at+1000 }] }] })
    fallback.observe(report(base,40),base)
    fallback.observe(report(base+60,50),base+60)
    const until = base+60+(minutes>=10080?1800:300)
    let latest = report(base+60,50)
    for (let at=base+90;at<until;at+=30) { latest=report(at,50); fallback.observe(latest,at) }
    latest = report(until,50)
    const a = latest.accounts[0], w = a.windows[0]
    assert.ok(new Forecasts().observe(latest,until)(a,w,until))
    assert.equal(fallback.observe(latest,until)(a,w,until),undefined)
  }
})

test("weekly calendar ETA includes repeated short pauses and matches both observations", () => {
  const r = snapshot(weeklyCalendarReport()), a = r.accounts[0], w = a.windows[1], now = r.checkedAt
  Schema.decodeUnknownSync(Snapshot)(r)
  const f = historicalForecast(a,w,now)!
  assert.equal(f.current!.limitAt,now+19800,"raw current pace is retained separately")
  const eta = (account: typeof a, at = now) => new Forecasts().observe({...r,accounts:[account]},at)(account,w,at)
  assert.equal(eta(a),"-10h 37m","average the live calendar ETA with the unchanged historical ETA")
  for (const change of [{resetAt:a.windows[0].resetAt!+1},{used:83},{observedAt:now-1},{minutes:480},{status:"stale"}]) {
    const changed = {...a,windows:[{...a.windows[0],...change},w]}
    assert.equal(eta(changed),"-8h 15m")
  }
  assert.equal(eta({...a,forecasts:[{...f,calendar:{...f.calendar!,validUntil:now}}]}),"-8h 15m")
  const resetFirst = {...f,calendar:{...f.calendar!,current:{percentPerHour:12,limitAt:null}}}
  assert.equal(eta({...a,forecasts:[resetFirst]}),undefined)
})

test("blocked short windows pause weekly instead of showing a resumed-use calendar promise", () => {
  const r = snapshot(weeklyCalendarReport()), a = r.accounts[0], w = a.windows[1], now = r.checkedAt
  const short = {...a.windows[0],used:100,remaining:0}
  const f = {...a.forecasts![0],calendar:{short,current:null,validUntil:null,pauses:0}}
  const changed = {...a,windows:[short,w],forecasts:[f]}
  const eta = new Forecasts().observe({...r,accounts:[changed]})
  assert.equal(eta(changed,w,now),undefined)
  assert.equal(eta(changed,{...w,used:100,remaining:0},now),undefined)
})

test("old weekly metadata and local fallback work without a calendar projection", () => {
  const r = snapshot(weeklyCalendarReport()), a = r.accounts[0], w = a.windows[1], now = r.checkedAt
  const f = {...a.forecasts![0],calendar:undefined}
  assert.equal(new Forecasts().observe({...r,accounts:[{...a,forecasts:[f]}]})(a,w,now),"-8h 15m")
  const fallback = new Forecasts()
  const earlier = {...w,observedAt:now-60,used:32,remaining:68}
  fallback.observe({...r,accounts:[{...a,forecasts:undefined,windows:[earlier]}]},now-60)
  const live = fallback.observe({...r,accounts:[{...a,forecasts:undefined,windows:[w]}]},now)
  assert.equal(live(a,w,now),"-33m")
})

test("quota calendar IPC rejects mismatched, expired or malformed evidence and strips unknown fields", () => {
  const raw = weeklyCalendarReport(), a = raw.quota_accounts[0], f = a.quota_forecast[0], c = f.calendar
  const withCalendar = (calendar:unknown) => ({...raw,quota_accounts:[{...a,quota_forecast:[{...f,calendar}]}]})
  for (const invalid of [
    {...c,pauses:-1},{...c,pauses:1.5},{...c,current:null},{...c,valid_until:f.observed_at},
    {...c,valid_until:f.current_valid_until+1},{...c,short:{...c.short,kind:f.kind}},
    {...c,short:{...c.short,used_percent:100}},{...c,short:{...c.short,observed_at:f.observed_at-1}},
    {...c,current:{percent_per_hour:0,limit_at:null}},{...c,current:{...c.current,limit_at:f.reset_at}},
  ]) assert.throws(()=>snapshot(withCalendar(invalid)))
  const safe = snapshot(withCalendar({...c,secret:"private-body-marker",short:{...c.short,secret:"private-body-marker"}}))
  assert.doesNotMatch(JSON.stringify(safe),/private-body-marker/)
  const only = snapshot(withCalendar({...c,short:null,pauses:0,current:f.current}))
  const onlyAccount = {...only.accounts[0],windows:[only.accounts[0].windows[1]]}
  assert.equal(new Forecasts().observe({...only,accounts:[onlyAccount]})(onlyAccount,onlyAccount.windows[0],raw.checked_at),"-8h 15m")
})
