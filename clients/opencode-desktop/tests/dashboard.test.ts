import assert from "node:assert/strict"
import { mkdtemp, writeFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { Schema } from "effect"
import { best, Forecasts, priority, routingFresh, visibleWindows } from "../src/dashboard"
import { activity, activityDate, activityIndex, activityValue } from "../src/chart"
import { NativeClient, snapshot, models, tokens, usage } from "../src/native"
import { options } from "../src/options"
import { Models, Profile, Tokens, Usage } from "../src/schemas"
import { deferred, doctor, modelList, tokenList, usage as usageFixture } from "./fixtures"

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
  await client.tokens(signal); await client.models(signal); await client.usage({ by: "total", period: "day" }, signal)
  assert.deepEqual(calls, [["token", "list", "--json"], ["models", "--json"], ["usage", "--period", "day", "--json"]])
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
    return { ...r, accounts: [{ ...a, windows: [{ ...w, observedAt: at, used, remaining: 100 - used, resetAt: base + 604800 }] }] }
  }
  forecasts.observe(report(base, 10), base)
  const active = report(base + 60, 12)
  forecasts.observe(active, base + 60)
  assert.equal(forecasts.label(1, active.accounts[0].windows[0], base + 60), "~44m to limit")
  assert.equal(forecasts.label(2, active.accounts[0].windows[0], base + 60), undefined)
  assert.equal(forecasts.label(1, active.accounts[0].windows[0], base + 120), undefined)
  for (let elapsed = 30; elapsed <= 300; elapsed += 30) {
    const r = report(base + 60 + elapsed, 12)
    forecasts.observe(r, base + 60 + elapsed)
    assert.equal(!!forecasts.label(1, r.accounts[0].windows[0], base + 60 + elapsed), elapsed < 300)
  }
  forecasts.observe(report(base + 390, 0), base + 390)
  assert.equal(forecasts.label(1, report(base + 390, 0).accounts[0].windows[0], base + 390), undefined)
})
