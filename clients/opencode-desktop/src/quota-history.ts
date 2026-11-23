import type { Account, QuotaHistory } from "./schemas"
import { duration } from "./format"

export const quotaColors = ["var(--exr-history-short)", "var(--exr-history-long)"]
export function quotaTimeline(account: Account) {
  const history = account.history
  if (!history) return { hours: [] as number[], series: [] as ReturnType<typeof series>[] }
  const interval = history.interval ?? 3600
  const hours = Array.from({ length: (history.to - history.from) / interval }, (_, i) => history.from + i * interval)
  const kinds = [...new Set(history.points.map((p) => p.kind))]
    .filter((kind) => (minutes(kind) ?? 0) > 0)
    .sort((a, b) => (minutes(a) ?? Infinity) - (minutes(b) ?? Infinity))
  function minutes(kind: string) { return [...history!.points].reverse().find((p) => p.kind === kind)?.minutes ?? account.windows.find((w) => w.kind === kind)?.minutes }
  const lines = kinds.map((kind, i) => series(history, hours, kind, minutes(kind), quotaColors[(minutes(kind) ?? 0) >= 10080 ? 1 : i % 2]))
  const platform = platformSeries(account, history, hours)
  if (platform) lines.push(platform)
  return { hours, series: lines }
}
function series(history: QuotaHistory, hours: number[], kind: string, minutes: number | null | undefined, color: string) {
  const samples = new Map(history.points.filter((p) => p.kind === kind).map((p) => [p.hour, p]))
  const points = new Map<number, number>()
  const segments: string[] = [], isolated: { x: number; y: number }[] = []
  let line: { x: number; y: number }[] = []
  const flush = () => { if (line.length === 1) isolated.push(line[0]); else if (line.length) segments.push(line.map((p) => `${p.x},${p.y}`).join(" ")); line = [] }
  for (const [i, hour] of hours.entries()) {
    const point = samples.get(hour)
    if (!point) { flush(); continue }
    if (i > 0 && samples.get(hours[i - 1])?.minutes !== point.minutes) flush()
    const left = quotaLeft(point.used)
    points.set(hour, left)
    line.push({ x: quotaX(i, hours.length), y: quotaY(left) })
  }
  flush()
  return { kind, label: duration(minutes ?? null), color, points, segments, isolated }
}
function platformSeries(account: Account, history: QuotaHistory, hours: number[]): ReturnType<typeof series> | undefined {
  const h = history.platformBalance, balance = account.platform?.balance
  if (!h?.points.length || !balance || balance.exetrouter.status === "configuration_changed" || h.enteredAt !== balance.enteredAt || h.initialUSD !== balance.initialUSD) return
  const points = new Map<number, number>()
  const path: string[] = []
  let left: number | undefined, next = 0, end = 0, change = 0
  for (const [i, hour] of hours.entries()) {
    if (hour > h.throughBucket) break
    const x = quotaX(i, hours.length)
    if (h.points[next]?.hour === hour) {
      if (left !== undefined) path.push(`${x},${quotaY(left)}`)
      left = h.points[next++].left
      path.push(`${x},${quotaY(left)}`)
      change = x
    }
    if (left !== undefined) { points.set(hour, left); end = x }
  }
  if (left === undefined) return
  const y = quotaY(left)
  if (end !== change) path.push(`${end},${y}`)
  return { kind: "platform", label: "Platform API", color: "var(--exr-history-api)", points,
    segments: points.size > 1 ? [path.join(" ")] : [], isolated: points.size === 1 ? [{ x: end, y }] : [] }
}
export function quotaX(index: number, length: number) { return 44 + index / Math.max(1, length - 1) * 600 }
export function quotaLeft(used: number) { return Math.min(100, Math.max(0, 100 - used)) }
export function quotaY(left: number) { return 204 - Math.min(100, Math.max(0, left)) / 100 * 184 }
