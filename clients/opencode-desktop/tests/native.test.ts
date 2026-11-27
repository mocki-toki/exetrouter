import assert from "node:assert/strict"
import { test } from "node:test"
import { Schema } from "effect"
import { platformRemaining } from "../src/dashboard"
import { execJSON, NativeClient, platformHistory, snapshot, usage } from "../src/native"
import { options } from "../src/options"
import { count, date, duration, freshness, percent, subscriptionPlan } from "../src/format"
import { Snapshot, Usage } from "../src/schemas"
import { deferred, doctor, usage as usageFixture } from "./fixtures"

test("normalizes both standalone and remote diagnostics without forwarding arbitrary fields", () => {
  const value = doctor()
  Object.assign(value, { secret: "private-body-marker" })
  const local = snapshot(value)
  assert.deepEqual(snapshot({ server: value }), local)
  assert.equal(local.accounts[0].windows[0].remaining, 0)
  assert.equal(local.accounts[0].windows[0].minutes, 480)
  assert.doesNotMatch(JSON.stringify(local), /private-body-marker/)
  Schema.decodeUnknownSync(Snapshot)(local)
  assert.throws(() => snapshot({ server: null }))
  assert.throws(() => snapshot({ ...value, schema_version: 99 }))
})

test("preserves nullable usage and hides token identifiers when a private label is unavailable", () => {
  const report = usageFixture()
  const result = usage(report, "model")
  assert.equal(result.rows[0].input, 0)
  assert.equal(result.rows[0].output, null)
  assert.equal(result.rows[0].total, null)
  assert.equal(result.rows[0].unknown, 1)
  Schema.decodeUnknownSync(Usage)(result)
  report.rows[0].name = "private-token-id"
  assert.equal(usage(report, "token").rows[0].name, "Label unavailable")
})

test("billing metadata is identical in remote and standalone projections and never fabricates credit", () => {
  const raw = doctor()
  const value = { ...raw, quota_accounts: [{ ...raw.quota_accounts[0], subscription: { billing_day: 8, plan_usd: 100, effective_api_budget_cents: 10000, secret: "private-body-marker" } }] }
  const result = snapshot(value)
  assert.deepEqual(snapshot({ server: value }), result)
  assert.deepEqual(result.accounts[0].subscription, { billingDay: 8, planUSD: 100, apiBudgetCents: 10000 })
  assert.equal(snapshot(raw).accounts[0].subscription, null)
  assert.doesNotMatch(JSON.stringify(result), /private-body-marker|remainingBalance/)
  for (const billing_day of [0, 32, 1.5]) assert.throws(() => snapshot({ ...value, quota_accounts: [{ ...value.quota_accounts[0], subscription: { billing_day } }] }))
})

test("account subscription text describes the configured plan, not Platform connectivity", () => {
  assert.equal(subscriptionPlan(20), "Plus $20")
  assert.equal(subscriptionPlan(100), "Pro $100")
  assert.equal(subscriptionPlan(200), "Pro $200")
  assert.equal(subscriptionPlan(5000), "$5k/month")
  for (const price of [null, undefined, 0, NaN]) assert.equal(subscriptionPlan(price), undefined)
})

test("Platform credential presence is optional, redacted and never an inferred credit balance", () => {
  const raw = doctor()
  const value = { ...raw, quota_accounts: [{ ...raw.quota_accounts[0], platform: { configured_at: 1800000000, credit_status: "unverified", inference_enabled: false, admin_key: "private-body-marker" } }] }
  const result = snapshot(value)
  assert.deepEqual(snapshot({ server: value }), result)
  assert.deepEqual(result.accounts[0].platform, { configuredAt: 1800000000, creditStatus: "unknown", inferenceEnabled: false })
  const current = { ...value, quota_accounts: [{ ...value.quota_accounts[0], platform: { ...value.quota_accounts[0].platform, credit_status: "unknown" } }] }
  assert.deepEqual(snapshot(current).accounts[0].platform, result.accounts[0].platform)
  assert.equal(snapshot(raw).accounts[0].platform, null)
  assert.doesNotMatch(JSON.stringify(result), /private-body-marker|admin_key|remainingBalance/)
  assert.equal(snapshot({ ...value, quota_accounts: [{ ...value.quota_accounts[0], platform: { ...value.quota_accounts[0].platform, inference_enabled: true } }] }).accounts[0].platform?.inferenceEnabled,true)
  assert.throws(() => snapshot({ ...value, quota_accounts: [{ ...value.quota_accounts[0], platform: { ...value.quota_accounts[0].platform, inference_enabled: "true" } }] }))
})

test("Platform Costs and daily history stay distinct from free credit and discard secrets", () => {
  const raw=doctor(), base=Math.floor(1800000000/86400)*86400
  const value={...raw,quota_accounts:[{...raw.quota_accounts[0],platform:{configured_at:base,inference_enabled:false,credit_status:"unverified",codex_fallback_allowed:false,costs:{from_utc:base,to_utc:base+86400,observed_at:base+86400,basis:"calendar_month_utc",spent_usd_nanos:5000e9,budget_cents:1000000,complete:true,secret:"private-body-marker"},key:{state:"active",created_at:base,expires_at:base+30*86400,last_used_at:null,checked_at:base+86400,secret:"private-body-marker"}}}]}
  const result=snapshot(value)
  assert.equal(result.accounts[0].platform?.costs?.spentUSD,5000)
  assert.equal(result.accounts[0].platform?.codexFallbackAllowed,false)
  assert.equal(result.accounts[0].platform?.key?.expiresAt,base+30*86400)
  assert.doesNotMatch(JSON.stringify(result),/private-body-marker/)
  const history=platformHistory({from_utc:base,to_utc:base+86400,interval_seconds:86400,points:[{from_utc:base,to_utc:base+86400,observed_at:base+86400,usd_nanos:0,secret:"private-body-marker"}]})
  assert.equal(history.points[0].usd,0)
  assert.doesNotMatch(JSON.stringify(history),/private-body-marker/)
  assert.throws(()=>platformHistory({from_utc:base,to_utc:base+86400,interval_seconds:600,points:[]}))
})

test("manual balance projects independent estimates, unknowns and overspend without secrets", () => {
  const raw=doctor(), at=1800000000
  const estimate={remaining_usd_nanos:99e9 as number|null,spent_usd_nanos:1e9,through_utc:at+100,observed_at:at+100,unpriced_requests:0,status:"estimated",secret:"private-body-marker"}
  const platform={configured_at:at,inference_enabled:true,credit_status:"unknown",balance:{entered_at:at,initial_usd_nanos:100e9,exetrouter:estimate,organization:{...estimate,remaining_usd_nanos:97e9,spent_usd_nanos:3e9,status:"delayed_report"},secret:"private-body-marker"}}
  const value={...raw,quota_accounts:[{...raw.quota_accounts[0],platform}]}
  const result=snapshot(value)
  assert.deepEqual(snapshot({server:value}),result)
  Schema.decodeUnknownSync(Snapshot)(result)
  assert.equal(result.accounts[0].platform?.balance?.exetrouter.remainingUSD,99)
  assert.equal(result.accounts[0].platform?.balance?.organization.remainingUSD,97)
  assert.doesNotMatch(JSON.stringify(result),/private-body-marker/)
  const withEstimate=(e:typeof estimate)=>snapshot({...value,quota_accounts:[{...value.quota_accounts[0],platform:{...platform,balance:{...platform.balance,exetrouter:e}}}]}).accounts[0].platform?.balance
  assert.equal(withEstimate({...estimate,remaining_usd_nanos:-1e9,spent_usd_nanos:101e9})?.exetrouter.remainingUSD,-1)
  const partial=withEstimate({...estimate,remaining_usd_nanos:null,unpriced_requests:1,status:"unpriced_requests"})
  assert.equal(partial?.exetrouter.remainingUSD,null)
  assert.equal(platformRemaining(partial),99)
  assert.equal(platformRemaining(withEstimate({...estimate,remaining_usd_nanos:null,spent_usd_nanos:0,unpriced_requests:2,status:"unpriced_requests"})),100)
  assert.equal(platformRemaining(withEstimate({...estimate,remaining_usd_nanos:null,unpriced_requests:1,status:"configuration_changed"})),null)
  assert.throws(()=>withEstimate({...estimate,unpriced_requests:1}))
  assert.throws(()=>withEstimate({...estimate,remaining_usd_nanos:100e9}))
  assert.throws(()=>withEstimate({...estimate,status:"confirmed_provider_balance"}))
})

test("formats unknowns, actual durations and historical quota honestly", () => {
  assert.equal(count(null), "Unknown")
  assert.equal(count(0), "0")
  assert.equal(percent(1), "1%")
  assert.equal(percent(1.23), "1.2%")
  assert.equal(date(null), "Unknown")
  assert.doesNotMatch(date(1800000000), /GMT|UTC/)
  assert.equal(duration(480), "8-hour")
  assert.equal(duration(10080), "Weekly")
  assert.equal(duration(null), "Unknown duration")
  const window = snapshot(doctor()).accounts[0].windows[0]
  assert.equal(freshness(window, window.observedAt), "current")
  assert.equal(freshness(window, window.observedAt + 60), "historical")
  assert.equal(freshness({ ...window, resetAt: window.observedAt }, window.observedAt), "historical")
})

test("read-only bridge uses the selected profile and never invokes live limits or mutations", async () => {
  const calls: string[][] = []
  const client = new NativeClient(options({ exrBinary: "/synthetic/exr", exrConfig: "/synthetic/profile.json" }), async (binary, args, _, allowNonzero) => {
    assert.equal(binary, "/synthetic/exr")
    calls.push([...args])
    assert.equal(args.at(-1), "--json")
    assert.deepEqual(args.slice(0, 2), ["--config", "/synthetic/profile.json"])
    if (args[2] === "doctor") { assert.equal(allowNonzero, true); return doctor() }
    return usageFixture()
  })
  const signal = new AbortController().signal
  await client.snapshot(signal)
  await client.usage({ period: "7d", by: "token" }, signal)
  assert.equal(calls.length, 2)
  assert.deepEqual(calls.map((args) => args.slice(2)), [["doctor", "--json"], ["usage", "--period", "7d", "--by", "token", "--json"]])
  await assert.rejects(client.usage({ period: "week; command" as "7d", by: "model" }, signal))
  assert.equal(calls.length, 2)
})

test("rolling usage and limit history forward exclusive ends only as validated passive arguments", async () => {
  const calls: string[][] = []
  const raw = doctor().quota_accounts[0].quota_history
  const client = new NativeClient(options({}), async (_, args) => { calls.push([...args]); return args[0] === "quota-history" ? raw : usageFixture() })
  const signal = new AbortController().signal
  await client.usage({period:"90d",by:"total",before:raw.to_utc},signal)
  await client.quotaHistory({account:1,period:"30d",before:raw.to_utc},signal)
  assert.deepEqual(calls,[["usage","--period","90d","--before",String(raw.to_utc),"--json"],["quota-history","1","--period","30d","--before",String(raw.to_utc),"--json"]])
  for (const before of [-1,NaN,Infinity,1.5,Number.MAX_SAFE_INTEGER+1]) {
    await assert.rejects(client.usage({period:"24h",by:"model",before},signal))
    await assert.rejects(client.quotaHistory({account:1,period:"7d",before},signal))
  }
  await assert.rejects(client.quotaHistory({account:1,period:"7d",before:raw.to_utc-1},signal))
  await assert.rejects(client.quotaHistory({account:0,period:"7d"},signal))
  assert.equal(calls.length,2,"invalid selections never launch subprocesses")
  await client.quotaHistory({account:1,period:"24h",before:raw.to_utc-600},signal)
  assert.equal(calls.at(-1)?.at(-2),String(raw.to_utc-600),"ten-minute page ends are valid")
})

test("refuses excessive concurrent reads instead of building a queue", async () => {
  const gate = deferred<unknown>()
  const client = new NativeClient(options({}), async () => gate.promise)
  const signal = new AbortController().signal
  const first = client.snapshot(signal)
  const second = client.snapshot(signal)
  const available = client.whenAvailable()
  assert.equal(client.whenAvailable(),available,"background refresh shares one bounded slot notification")
  await assert.rejects(client.snapshot(signal), /busy/)
  gate.resolve(doctor())
  await Promise.all([first, second])
  await available
})

test("subprocess JSON accepts nonzero doctor diagnostics only when explicitly requested", async () => {
  const args = ["-e", 'process.stdout.write("{\\"valid\\":true}"); process.exitCode=1']
  const signal = new AbortController().signal
  assert.deepEqual(await execJSON(process.execPath, args, signal, true), { valid: true })
  await assert.rejects(execJSON(process.execPath, args, signal), /read failed/)
  await assert.rejects(execJSON(process.execPath, ["-e", 'process.stdout.write("private-body-marker")'], signal), (e: Error) => !e.message.includes("private-body-marker"))
})

test("bounds subprocess output, deadline and cancellation without exposing stderr", async () => {
  const signal = new AbortController().signal
  await assert.rejects(execJSON(process.execPath, ["-e", 'process.stderr.write("private-body-marker"); process.stdout.write("x".repeat(10000))'], signal, false, 5000, 100), /read failed/)
  await assert.rejects(execJSON(process.execPath, ["-e", "setInterval(()=>{},1000)"], signal, false, 100), /read failed/)
  const controller = new AbortController()
  const pending = execJSON(process.execPath, ["-e", "setInterval(()=>{},1000)"], controller.signal)
  controller.abort()
  await assert.rejects(pending, /read failed/)
  await assert.rejects(execJSON(process.execPath, [], controller.signal), /cancelled/)
})
