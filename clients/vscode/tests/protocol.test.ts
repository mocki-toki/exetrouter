import assert = require("node:assert/strict")
import { test } from "node:test"
import { createServer, type RequestListener } from "node:http"
import { once } from "node:events"
import { AddressInfo } from "node:net"
import { baseUrl, catalog, discover, secretKey } from "../src/catalog"
import { accountRows, statusSummary, tokenRows } from "../src/dashboard"
import { quotaChart, usageChart } from "../src/charts"
import { generate, history, type Emission } from "../src/responses"

async function fixture(handler: RequestListener) {
  const server = createServer(handler)
  server.listen(0, "127.0.0.1")
  await once(server, "listening")
  return {
    base: `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1`,
    close() {
      server.closeAllConnections()
      server.close()
    },
  }
}
const frame = (event: unknown) => `data: ${JSON.stringify(event)}\r\n\r\n`
const done = (output: unknown[] = []) =>
  frame({ type: "response.completed", response: { status: "completed", output } })

test("complete Responses history and tool call identity survive streamed continuation", async (t) => {
  const call = {
    type: "function_call",
    id: "item-7",
    call_id: "call-42",
    name: "find",
    arguments: '{"q":"birds"}',
  }
  const requests: Record<string, any>[] = []
  const api = await fixture(async (req, res) => {
    assert.equal(req.url, "/v1/responses")
    assert.equal(req.headers.authorization, "Bearer fixture")
    const parts = []
    for await (const part of req) parts.push(part)
    requests.push(JSON.parse(Buffer.concat(parts).toString()))
    res.writeHead(200, { "content-type": "text/event-stream" })
    if (requests.length === 1) {
      const out =
        frame({ type: "response.output_item.added", output_index: 0, item: { ...call, arguments: "" } }) +
        frame({ type: "response.function_call_arguments.delta", output_index: 0, delta: '{"q"' }) +
        frame({ type: "response.function_call_arguments.done", output_index: 0, arguments: call.arguments }) +
        frame({ type: "response.output_item.done", output_index: 0, item: call }) +
        done([call])
      res.write(out.slice(0, 17))
      res.end(out.slice(17))
    } else
      res.end(
        frame({ type: "response.output_text.delta", output_index: 0, content_index: 0, delta: "found" }) +
          done([{ type: "message", content: [{ type: "output_text", text: "found it" }] }]),
      )
  })
  t.after(api.close)
  const question = { role: "user" as const, parts: [{ type: "input_text" as const, text: "Search" }] }
  const got: Emission[] = []
  await generate(
    api.base,
    "fixture",
    { model: "synthetic", input: history([question]) },
    (p) => got.push(p),
    new AbortController().signal,
  )
  assert.deepEqual([...got], [{ id: "call-42", name: "find", input: { q: "birds" } }])
  const tool = got[0]
  if (!("id" in tool)) throw Error()
  await generate(
    api.base,
    "fixture",
    {
      model: "synthetic",
      input: history([
        question,
        {
          role: "assistant",
          parts: [
            {
              type: "function_call",
              call_id: tool.id,
              name: tool.name,
              arguments: JSON.stringify(tool.input),
            },
          ],
        },
        {
          role: "user",
          parts: [
            { type: "function_call_output", call_id: tool.id, output: "result" },
            { type: "input_text", text: "Summarize" },
          ],
        },
      ]),
    },
    (p) => got.push(p),
    new AbortController().signal,
  )
  assert.deepEqual(
    requests[1].input.map((p: any) => p.call_id ?? p.role),
    ["user", "call-42", "call-42", "user"],
  )
  assert.equal(requests[1].store, false)
  assert.equal(requests[1].previous_response_id, undefined)
  assert.equal(requests[1].max_output_tokens, undefined)
  assert.deepEqual(got.slice(1), [{ text: "found" }, { text: " it" }])
})

test("cancelling one in-flight POST leaves its parallel sibling intact", { timeout: 5000 }, async (t) => {
  const stop = new AbortController()
  let start!: () => void, finish!: () => void
  const waiting = new Promise<void>((r) => (start = r)),
    closed = new Promise<void>((r) => (finish = r))
  let requests = 0
  const api = await fixture((_req, res) => {
    requests++
    res.writeHead(200, { "content-type": "text/event-stream" })
    if (requests === 1) {
      res.write(": ping\n\n")
      res.on("close", finish)
      start()
    } else res.end(done())
  })
  t.after(api.close)
  const first = generate(api.base, "fixture", {}, () => {}, stop.signal)
  await waiting
  await generate(api.base, "fixture", {}, () => {}, new AbortController().signal)
  stop.abort()
  await assert.rejects(first)
  await closed
  assert.equal(requests, 2)
})

test("rejects interrupted streams, redacts upstream errors and never follows redirects", async (t) => {
  let requests = 0
  const api = await fixture((_req, res) => {
    requests++
    if (requests === 1) {
      res.writeHead(200, { "content-type": "text/event-stream" })
      res.end("data: [DONE]\n\n")
    } else if (requests === 2) {
      res.writeHead(403)
      res.end("private fixture payload")
    } else {
      res.writeHead(307, { location: "/other" })
      res.end()
    }
  })
  t.after(api.close)
  const run = () => generate(api.base, "fixture", {}, () => {}, new AbortController().signal)
  await assert.rejects(run, /before completion/)
  await assert.rejects(run, /^Error: ExetRouter token rejected \(HTTP 403\)/)
  await assert.rejects(discover(api.base, "fixture", new AbortController().signal))
  assert.equal(requests, 3)
})

test("catalog projects actual limits, routes and capabilities; secret keys isolate URL paths", () => {
  const metadata = {
    context_window: 12000,
    auto_compact_token_limit: 10000,
    input_modalities: ["text", "image"],
    supports_parallel_tool_calls: true,
    supported_reasoning_levels: [{ effort: "high" }],
  }
  const models = catalog({
    data: [
      { id: "one", exetrouter: metadata, capabilities: ["tools", "image_input"], platform_fallback: true },
      { id: "two", source: "platform", exetrouter: { ...metadata, max_output_tokens: 2000 } },
      { id: "embed", source: "platform", capabilities: ["embeddings"] },
    ],
  })
  assert.deepEqual(
    models.map((m) => [m.route, m.chat, m.input, m.output, m.tools, m.vision]),
    [
      ["Codex · Platform fallback", true, 10000, undefined, true, true],
      ["Platform API", true, 10000, 2000, false, false],
      ["Platform API", false, undefined, undefined, false, false],
    ],
  )
  assert.deepEqual(models[0].efforts, ["high"])
  assert.equal(models[0].parallelTools, true)
  assert.equal(models[2].parallelTools, false)
  assert.notEqual(secretKey("https://router.example/a/v1"), secretKey("https://router.example/b/v1"))
  assert.notEqual(secretKey("https://router.example/v1"), secretKey("https://other.example/v1"))
  assert.throws(() => baseUrl("http://router.example/v1"))
  assert.throws(() => baseUrl("https://user@router.example/v1"))
  assert.throws(() => baseUrl("https://router.example/responses"))
})

test("catalog reports authorization, wrong endpoint, and invalid JSON without exposing the response", async (t) => {
  let request = 0
  const api = await fixture((_req, res) => {
    res.writeHead([401, 404, 200][request++])
    res.end("private synthetic response")
  })
  t.after(api.close)
  const fetch = () => discover(api.base, "fixture", new AbortController().signal)
  await assert.rejects(fetch, { message: "API token rejected (HTTP 401).", status: 401 })
  await assert.rejects(fetch, { message: "The configured API address does not serve /v1/models.", status: 404 })
  await assert.rejects(fetch, { message: "The configured endpoint did not return a valid model catalog." })
})

test("distinguishes router quota failures and preserves upstream error privacy", async (t) => {
  let requests = 0
  const api = await fixture((_req, res) => {
    if (++requests === 1) {
      res.writeHead(429)
      res.end("synthetic private quota message")
    } else {
      res.writeHead(200, { "content-type": "text/event-stream" })
      res.end(frame({ type: "error", error: { code: "upstream_interrupted", message: "synthetic private response" } }))
    }
  })
  t.after(api.close)
  const run = () => generate(api.base, "fixture", {}, () => {}, new AbortController().signal)
  await assert.rejects(run, { message: "ExetRouter quota or concurrency limit reached." })
  await assert.rejects(run, { message: "The upstream response was interrupted. ExetRouter did not retry the request." })
  assert.equal(requests, 2)
})

test("all account statuses remain visible, including unknowns and disabled accounts", () => {
  const rows = accountRows({
    server: {
      schema_version: 1,
      quota_accounts: [
        {
          label: "First",
          routing: { best: true },
          quota: {
            status: "current",
            windows: [{ kind: "primary", remaining_percent: null, status: "stale" }],
          },
        },
        { label: "Second", preference: { enabled: false }, quota: { status: "current" } },
        { label: "Third", quota: { status: "cooldown" } },
      ],
    },
  })
  assert.deepEqual(
    rows.map((r) => r.description),
    ["current", "Disabled", "cooldown"],
  )
  assert.match(rows[0].detail!, /Unknown remaining · stale/)
  assert.equal(statusSummary(rows), "3 accounts · 3 states")
})

test("account quota meters show Platform percentage above the manual dollar estimate", () => {
  const [row] = accountRows({ schema_version: 1, quota_accounts: [{ id: 1,
    quota: { windows: [{ window_minutes: 10080, remaining_percent: 72, status: "current" }] },
    platform: { balance: { initial_usd_nanos: 86e9,
      exetrouter: { remaining_usd_nanos: null, spent_usd_nanos: 43e9, unpriced_requests: 1, status: "unpriced_requests" },
    } },
  }] })
  assert.deepEqual(row.meters?.map((m) => [m.label, m.remaining, m.caption]), [
    ["Weekly", 72, "Resets Unknown"], ["Platform API · partial estimate", 50, "$43 / $86"],
  ])
  assert.deepEqual(row.children?.at(-1)?.args, [1])
  assert.equal(row.children?.at(-1)?.description, "50% left")
  const [unknown] = accountRows({ schema_version: 1, quota_accounts: [{ platform: { balance: null } }] })
  assert.equal(unknown.meters?.[0]?.remaining, null)
  assert.equal(unknown.meters?.[0]?.caption, "Unknown")
})

test("usage charts keep missing counters unknown, distinguish zero and flag unpriced estimates", () => {
  const row = { name: "one", requests: 1, total_tokens: null, cached_input_tokens: 0, unknown_usage: 1, unpriced_requests: 1 }
  const report = { from_utc: 100, to_utc: 400, rows: [row], timeline: [
    { from_utc: 100, to_utc: 200, rows: [row] }, { from_utc: 300, to_utc: 400, rows: [] },
  ] }
  assert.deepEqual(usageChart(report, "tokens").series[0]?.points, [[100, null, false], [300, 0, true]])
  assert.deepEqual(usageChart(report, "requests").series[0]?.points, [[100, 1, false], [300, 0, true]])
  assert.equal(usageChart(report, "tokens").breakdown?.[0]?.cached, 0)
  assert.equal(usageChart(report, "tokens").breakdown?.[0]?.output, null)
  assert.match(usageChart(report, "usd").summary!, /no price estimate/)
})

test("quota chart uses actual durations and leaves gaps and Platform estimate boundaries intact", () => {
  const chart = quotaChart({ from_utc: 0, to_utc: 5000, interval_seconds: 600, points: [
    { hour: 0, kind: "primary", used_percent: 0, window_minutes: 480 },
    { hour: 600, kind: "primary", used_percent: 50, window_minutes: 480 },
    { hour: 600, kind: "secondary", used_percent: 80, window_minutes: 10080 },
    { hour: 1800, kind: "primary", used_percent: 100, window_minutes: 480 },
  ], platform_balance: { through_bucket: 2400, points: [{ hour: 600, remaining_percent: 99 }, { hour: 1200, remaining_percent: 90 }] } })
  assert.equal(chart.series[0]?.label, "8-hour window")
  assert.deepEqual(chart.series[0]?.points, [[0, 100, false], [600, 50, false], [1800, 0, true]])
  assert.equal(chart.series[1]?.label, "Weekly")
  assert.deepEqual(chart.series[2]?.points, [[600, 99], [1200, 90], [2400, 90]])
  assert.equal(chart.series[2]?.step, true)
})

test("token rows separate lifecycle status and the saved Fast override", () => {
  const [a, b, c] = tokenRows([{ fast_mode: true }, { fast_mode: false, disabled: true }, {}])
  assert.equal(a.description, "Active · Fast allowed")
  assert.equal(b.tokenState, "Disabled")
  assert.equal(b.description, "Disabled · Fast blocked")
  assert.equal(c.fastMode, null)
})
