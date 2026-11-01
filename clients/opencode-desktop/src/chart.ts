import type { Usage } from "./schemas"
import { date } from "./format"

export function activityDate(bucket: Usage["timeline"][number], period: string): string {
  return period === "day" || period === "24h" ? date(bucket.to)
    : new Date(bucket.from * 1000).toLocaleDateString("en", { month: "short", day: "numeric" })
}

export const chartColors = ["#84becb", "#aaa3e6", "#8fc9a4", "#e4bd79", "#e39393", "#85a8e1", "#cf9dc6", "#c5c48d"]
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
    const values = new Map<string, number | null>()
    for (const row of bucket.rows) {
      const value = metric === "requests" ? row.requests : row.input === null && row.output === null ? null : (row.input ?? 0) + (row.output ?? 0)
      values.set(row.key, value === null ? values.get(row.key) ?? null : (values.get(row.key) ?? 0) + value)
    }
    return values
  })
  // Color follows report identity, not the selected subset or ranking order.
  const keys = [...new Set(report.rows.map((r) => r.key))].sort()
  const series = rows.map((row) => ({ key: row.key, name: row.name, color: chartColors[(keys.indexOf(row.key) + (metric === "tokens" ? 1 : 0)) % chartColors.length], values: indexed.map((b) => b.has(row.key) ? b.get(row.key)! : 0) }))
  let maximum = 1
  for (const line of series) for (const value of line.values) if (value !== null) maximum = Math.max(maximum, value)
  const magnitude = 10 ** Math.floor(Math.log10(maximum))
  maximum = ([1, 2, 5, 10].find((n) => n * magnitude >= maximum) ?? 10) * magnitude
  return {
    rows, buckets, maximum,
    requests: rows.reduce((n, r) => n + r.requests, 0),
    reported: rows.reduce((n, r) => n + (r.input ?? 0) + (r.output ?? 0), 0),
    incomplete: rows.reduce((n, r) => n + r.unknown, 0),
    series: series.map((line) => {
      const segments: string[] = [], isolated: {x:number;y:number}[] = []
      let points: {x:number;y:number}[] = []
      const flush = () => { if (points.length === 1) isolated.push(points[0]); else if (points.length) segments.push(points.map((p)=>`${p.x},${p.y}`).join(" ")); points=[] }
      line.values.forEach((v,index)=>{if(v===null){flush();return}points.push({x:44+index/Math.max(1,buckets.length-1)*600,y:204-v/maximum*184})})
      flush()
      return {...line,segments,isolated,points:segments.join(" ")}
    }),
  }
}
