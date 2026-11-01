import type { Usage } from "./schemas"
import { date } from "./format"

export function activityDate(bucket: Usage["timeline"][number], period: string): string {
  return period === "day" || period === "24h" ? date(bucket.to)
    : new Date(bucket.from * 1000).toLocaleDateString("en", { month: "short", day: "numeric" })
}

export const chartColors = ["var(--exr-accent)", "var(--exr-purple)", "var(--exr-success)", "var(--exr-warning)", "var(--exr-danger)"]
export function compact(value: number): string { return new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 }).format(value) }

/** SVG plot coordinates, independent of CSS width and device pixel ratio. */
export function activityIndex(length: number, clientX: number, left: number, width: number): number | undefined {
  if (!length || width <= 0) return
  const position = (clientX - left) / width * 668
  return Math.round(Math.max(0, Math.min(1, (position - 44) / 600)) * (length - 1))
}

export function activityValue(bucket: Usage["timeline"][number], key: string, metric: "tokens" | "requests") {
  const rows = bucket.rows.filter((r) => r.key === key)
  if (metric === "requests") return { value: rows.reduce((n, r) => n + r.requests, 0), incomplete: false }
  const incomplete = rows.some((r) => r.input === null || r.output === null || r.unknown > 0 || r.partial > 0)
  const unknown = rows.length > 0 && rows.every((r) => r.input === null && r.output === null)
  return { value: unknown ? null : rows.reduce((n, r) => n + (r.input ?? 0) + (r.output ?? 0), 0), incomplete }
}

/** Pre-index each bucket once, rather than rescanning every row for every SVG point. */
export function activity(report: Usage, selected: string, metric: "tokens" | "requests", now: number) {
  const rows = report.rows.filter((r) => !selected || r.key === selected)
  const buckets = report.timeline.filter((b) => b.from <= now)
  const indexed = buckets.map((bucket) => {
    const values = new Map<string, number>()
    for (const row of bucket.rows) values.set(row.key, (values.get(row.key) ?? 0) + (metric === "requests" ? row.requests : (row.input ?? 0) + (row.output ?? 0)))
    return values
  })
  const series = rows.map((row) => ({ key: row.key, name: row.name, values: indexed.map((b) => b.get(row.key) ?? 0) }))
  let maximum = 1
  for (const line of series) for (const value of line.values) maximum = Math.max(maximum, value)
  const magnitude = 10 ** Math.floor(Math.log10(maximum))
  maximum = ([1, 2, 5, 10].find((n) => n * magnitude >= maximum) ?? 10) * magnitude
  return {
    rows, buckets, maximum,
    requests: rows.reduce((n, r) => n + r.requests, 0),
    reported: rows.reduce((n, r) => n + (r.input ?? 0) + (r.output ?? 0), 0),
    incomplete: rows.reduce((n, r) => n + r.unknown, 0),
    series: series.map((line, i) => ({ ...line, color: chartColors[i % chartColors.length], points: line.values.map((v, index) => `${44 + index / Math.max(1, buckets.length - 1) * 600},${178 - v / maximum * 154}`).join(" ") })),
  }
}
