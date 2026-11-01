import assert from "node:assert/strict"
import { test } from "node:test"
import { Schema } from "effect"
import { execJSON, NativeClient, snapshot, usage } from "../src/native"
import { options } from "../src/options"
import { count, date, duration, freshness, percent } from "../src/format"
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
  await client.usage({ period: "week", by: "token" }, signal)
  assert.equal(calls.length, 2)
  assert.deepEqual(calls.map((args) => args.slice(2)), [["doctor", "--json"], ["usage", "--period", "week", "--by", "token", "--json"]])
  await assert.rejects(client.usage({ period: "week; command" as "week", by: "model" }, signal))
  assert.equal(calls.length, 2)
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
