import { randomBytes } from "node:crypto"
import type { Uri, Webview } from "vscode"
import { record } from "./catalog"

export const metrics = ["tokens", "requests", "usd"] as const
export type Metric = (typeof metrics)[number]
export type Point = [time: number, value: number | null, gap?: boolean]
export interface Chart {
  from: number
  to: number
  kind: "usage" | "quota"
  metric: Metric | "percent"
  series: { label: string; points: Point[]; step?: boolean }[]
  summary?: string
  breakdown?: { label: string; value: number | null; unknown: number | null; input: number | null; cached: number | null; output: number | null }[]
}
const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null
const title = (value: unknown) => typeof value === "string" && value ? value.slice(0, 160) : "Total"
function range(r: Record<string, unknown>) {
  const from = number(r.from_utc), to = number(r.to_utc)
  if (from === null || to === null || to <= from || to - from > 92 * 86400) throw new Error("Invalid report range.")
  return { from, to }
}

export function usageChart(value: unknown, metric: Metric): Chart {
  const r = record(value), bounds = range(r)
  if (!Array.isArray(r.timeline) || r.timeline.length > 256 || !Array.isArray(r.rows)) throw new Error("Usage timeline unavailable.")
  const measure = (row: Record<string, unknown>) => number(row[{
    tokens: "total_tokens", requests: "requests", usd: "estimated_usd",
  }[metric]])
  const key = (row: Record<string, unknown>) => typeof row.name === "string" ? row.name : ""
  const totals = r.rows.map(record).sort((a, b) => (measure(b) ?? -1) - (measure(a) ?? -1))
  const series = totals.slice(0, 8).map((row) => ({ id: key(row), label: title(row.display_name ?? row.name), points: [] as Point[] }))
  let priorEnd = bounds.from
  for (const raw of r.timeline) {
    const bucket = record(raw), at = number(bucket.from_utc), end = number(bucket.to_utc)
    if (at === null || end === null || at < priorEnd || end > bounds.to || end <= at || !Array.isArray(bucket.rows)) throw new Error("Invalid usage bucket.")
    const values = new Map(bucket.rows.map((raw) => { const row = record(raw); return [key(row), row] }))
    for (const line of series) {
      const row = values.get(line.id)
      line.points.push([at, row ? measure(row) : 0, at > priorEnd])
    }
    priorEnd = end
  }
  const incomplete = totals.reduce((n, r) => n + (number(r[metric === "usd" ? "unpriced_requests" : "unknown_usage"]) ?? 0)
    + (metric === "tokens" ? number(r.partial_usage) ?? 0 : 0), 0)
  return {
    ...bounds, kind: "usage", metric,
    series: series.map(({ label, points }) => ({ label, points })),
    summary: [
      totals.length > series.length ? `Chart shows the top ${series.length} groups.` : "",
      incomplete && metric !== "requests" ? `${incomplete.toLocaleString()} requests have ${metric === "usd" ? "no price estimate" : "missing or partial token counts"}.` : "",
    ].filter(Boolean).join(" "),
    breakdown: totals.slice(0, 100).map((row) => ({ label: title(row.display_name ?? row.name), value: measure(row), unknown: number(row.unknown_usage), input: number(row.input_tokens), cached: number(row.cached_input_tokens), output: number(row.output_tokens) })),
  }
}

export function quotaChart(value: unknown): Chart {
  const r = record(value), bounds = range(r), interval = number(r.interval_seconds)
  if ((interval !== 600 && interval !== 3600) || !Array.isArray(r.points) || r.points.length > 25920) throw new Error("Quota history unavailable.")
  const lines = new Map<string, { label: string; points: Point[] }>()
  for (const raw of r.points) {
    const p = record(raw), at = number(p.hour), used = number(p.used_percent), duration = number(p.window_minutes)
    if (at === null || at < bounds.from || at >= bounds.to || (p.kind !== "primary" && p.kind !== "secondary")) continue
    const label = duration === 10080 ? "Weekly" : duration && duration % 60 === 0 ? `${duration / 60}-hour window` : duration ? `${duration}-min window` : `${p.kind} window`
    const id = `${p.kind}:${duration}`, line = lines.get(id) ?? { label, points: [] }
    const prev = line.points.at(-1)
    if (prev && at <= prev[0]) continue
    line.points.push([at, used !== null && used >= 0 && used <= 100 ? 100 - used : null, !!prev && at - prev[0] > interval])
    if (!lines.has(id) && lines.size < 16) lines.set(id, line)
  }
  const series: Chart["series"] = [...lines.values()]
  const balance = record(r.platform_balance), until = number(balance.through_bucket)
  if (Array.isArray(balance.points) && balance.points.length <= 12960 && until !== null) {
    const points: Point[] = []
    for (const raw of balance.points) {
      const p = record(raw), time = number(p.hour), fraction = number(p.remaining_percent)
      if (time !== null && time >= bounds.from && time < bounds.to && time <= until && fraction !== null && fraction >= 0 && fraction <= 100 && (!points.length || time > points.at(-1)![0])) points.push([time, fraction])
    }
    const last = points.at(-1)
    if (last) {
      const end = Math.min(until, bounds.to)
      if (end > last[0]) points.push([end, last[1]])
      series.push({ label: "Platform balance estimate", points, step: true })
    }
  }
  return { ...bounds, kind: "quota", metric: "percent", series, summary: "Missing observations are shown as gaps." }
}

export function chartDocument(webview: Webview, script: Uri, css: Uri, kind: Chart["kind"]) {
  const nonce = randomBytes(18).toString("base64")
  // All report content crosses postMessage as data; no account or model string is HTML.
  return `<!doctype html><html lang="en"><head><meta charset="utf-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}'">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="stylesheet" href="${webview.asWebviewUri(css)}"><title>ExetRouter chart</title></head>
  <body data-kind="${kind}"><main><header><div class="heading"><h1 id="title">${kind === "usage" ? "Usage" : "Quota history"}</h1>
  <p id="range"></p></div><button id="refresh" type="button">Refresh</button></header>
  <div id="controls"><label>Period <select id="period"><option>24h</option><option selected>7d</option><option>30d</option><option>90d</option></select></label>
  ${kind === "usage" ? `<label>Group <select id="group"><option value="total">Total</option><option value="model" selected>Model</option><option value="user">User</option><option value="token">Token</option></select></label>
  <label>Metric <select id="metric"><option value="tokens">Tokens</option><option value="requests">Requests</option><option value="usd">API-equivalent USD</option></select></label>` : ""}</div>
  <p id="notice" role="status">Loading…</p><div id="legend"></div>
  <section id="plot" aria-label="Interactive chart" hidden><svg id="chart" tabindex="0" aria-label="Chart. Use Left and Right to inspect values"></svg><div id="cursor" role="status"></div></section>
  <div id="breakdown"></div></main><script nonce="${nonce}" src="${webview.asWebviewUri(script)}"></script></body></html>`
}
