import type { Account, QuotaHistory } from "./schemas"
import { duration } from "./format"

export const quotaColors = ["var(--exr-history-short)", "var(--exr-history-long)"]
export function quotaTimeline(account: Account) {
  const history = account.history
  if (!history) return { hours: [] as number[], series: [] as ReturnType<typeof series>[], latest: null }
  const hours = Array.from({ length: (history.to - history.from) / 3600 }, (_, i) => history.from + i * 3600)
  const kinds = [...new Set(history.points.map((p) => p.kind))]
    .sort((a, b) => (minutes(a) ?? Infinity) - (minutes(b) ?? Infinity))
  function minutes(kind: string) { return [...history!.points].reverse().find((p) => p.kind === kind)?.minutes ?? account.windows.find((w) => w.kind === kind)?.minutes }
  return { hours, series: kinds.map((kind, i) => series(history, hours, kind, minutes(kind), quotaColors[(minutes(kind) ?? 0) >= 10080 ? 1 : i % 2])), latest: history.points.reduce<number | null>((at, p) => Math.max(at ?? 0, p.observedAt), null) }
}
function series(history: QuotaHistory, hours: number[], kind: string, minutes: number | null | undefined, color: string) {
  const points = new Map(history.points.filter((p) => p.kind === kind).map((p) => [p.hour, p]))
  const segments: string[] = [], isolated: { x: number; y: number }[] = []
  let line: { x: number; y: number }[] = []
  const flush = () => { if (line.length === 1) isolated.push(line[0]); else if (line.length) segments.push(line.map((p) => `${p.x},${p.y}`).join(" ")); line = [] }
  for (const [i, hour] of hours.entries()) {
    const point = points.get(hour)
    if (!point) { flush(); continue }
    if (i > 0 && points.get(hours[i - 1])?.minutes !== point.minutes) flush()
    line.push({ x: quotaX(i, hours.length), y: quotaY(point.used) })
  }
  flush()
  return { kind, label: duration(minutes ?? null), color, points, segments, isolated }
}
export function quotaX(index: number, length: number) { return 44 + index / Math.max(1, length - 1) * 600 }
export function quotaY(used: number) { return 204 - Math.min(100, Math.max(0, used)) / 100 * 184 }
