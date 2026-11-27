import type { Account, PlatformBalance, Snapshot, Window, QuotaForecast } from "./schemas"

export function fresh(at: number | null, now: number): boolean { return at !== null && at <= now && now - at < 60 }
export function routingFresh(account: Account, now: number): boolean {
  return fresh(account.routing.observedAt, now) && (account.routing.validUntil === null || now < account.routing.validUntil)
}
export function best(account: Account, now: number): boolean { return account.routing.best && account.enabled !== false && routingFresh(account, now) }
export function routingReason(account: Account): string {
  return ({ reset_expiring: "Expiring reset credit", weekly_reset_soon: "Weekly quota resets within 24 hours", short_window_first: "Short-window capacity first", reserve: "Weekly-only reserve" } as Record<string, string>)[account.routing.reason] ?? "Metadata unknown"
}
export function priority(account: Account): boolean {
  return account.enabled !== false && ["reset_expiring", "weekly_reset_soon"].includes(account.routing.reason)
}
export function visibleWindows(account: Account): readonly Window[] {
  return account.windows.filter((w) => w.minutes !== null && w.minutes > 0).sort((a, b) => a.minutes! - b.minutes!)
}

export function platformRemaining(balance: typeof PlatformBalance.Type | null | undefined) {
  if (!balance) return null
  const estimate = balance.exetrouter
  if (estimate.remainingUSD !== null) return estimate.remainingUSD
  if (estimate.status === "unpriced_requests" && estimate.unpriced > 0 && estimate.spentUSD !== null) {
    return balance.initialUSD - estimate.spentUSD
  }
  return null
}

export function platformPercent(remainingUSD: number | null, initialUSD: number | undefined) {
  return remainingUSD !== null && initialUSD !== undefined && initialUSD > 0
    ? Math.max(0, Math.min(100, remainingUSD / initialUSD * 100))
    : null
}

/** Match the runtime's projections to the quota observation they describe. */
export function historicalForecast(account: Account, window: Window, now: number): QuotaForecast | undefined {
  if (account.refreshError || window.status !== "current" || !fresh(window.observedAt, now)) return
  const f = account.forecasts?.find((f) => f.kind === window.kind && f.minutes === window.minutes && f.resetAt === window.resetAt && f.observedAt === window.observedAt && f.used === window.used && f.resetAt > now)
  if (!f?.calendar) return f
  const s = f.calendar.short
  const matches = s ? account.windows.some((w) => w.kind === s.kind && w.minutes === s.minutes && w.resetAt === s.resetAt && w.observedAt === s.observedAt && w.used === s.used && w.status === "current" && fresh(w.observedAt,now) && w.resetAt !== null && w.resetAt > now) : account.windows.length === 1
  return matches ? f : {...f,calendar:undefined}
}
function calendarProjection(f: QuotaForecast | undefined, now: number) {
  const c = f?.calendar, s = c?.short
  return c?.validUntil != null && now < c.validUntil && (!s || s.status === "current" && fresh(s.observedAt,now) && s.resetAt !== null && s.resetAt > now) ? c.current : undefined
}
const trendKey = (account: Account, window: Window) => JSON.stringify([account.observationKey ?? account.label, account.id, window.kind])
export type ForecastLookup = (account: Account, window: Window, now: number) => string | undefined

/** Snapshot-local labels and a bounded live trend for older routers. */
export class Forecasts {
  private windows = new Map<string, { minutes: number; reset: number; change: number | null; samples: { at: number; used: number }[] }>()
  private sample(window: Window, now: number) {
    return window.status === "current" && fresh(window.observedAt, now) && window.used !== null && window.minutes !== null && window.minutes > 0 && window.resetAt !== null && window.resetAt > now
      ? { at: window.observedAt, used: window.used, minutes: window.minutes, reset: window.resetAt } : undefined
  }
  private isIdle(key: string, window: Window, now: number): boolean {
    const point = this.sample(window, now), trend = this.windows.get(key), last = trend?.samples.at(-1)
    if (!point || point.used >= 100 || !trend || !last || trend.minutes !== point.minutes || trend.reset !== point.reset || last.at !== point.at || last.used !== point.used) return false
    const idle = point.minutes >= 10080 ? 1800 : 300
    return trend.change !== null && now - trend.change >= idle || trend.samples[0].used === last.used && last.at - trend.samples[0].at >= idle
  }
  observe(report: Snapshot, now = report.checkedAt): ForecastLookup {
    const present = new Set<string>()
    const estimates = new Map<string, { at: number; window: Window; label: string }>()
    for (const account of report.accounts) {
      if (account.refreshError) continue
      for (const window of account.windows) {
        if (!["primary", "secondary"].includes(window.kind)) continue
        const key = trendKey(account, window)
        present.add(key)
        const point = this.sample(window, now)
        if (!point) { this.windows.delete(key); continue }
        this.observeWindow(key, point)
        if (this.isIdle(key, window, now)) continue
        const at = this.deadline(account, window, key, now)
        if (at === undefined) continue
        const minutes = Math.ceil((at - now) / 60)
        const label = `-${minutes >= 60 ? `${Math.floor(minutes / 60)}h ` : ""}${minutes % 60}m`
        estimates.set(key, { at, window, label })
      }
    }
    for (const key of this.windows.keys()) if (!present.has(key)) this.windows.delete(key)
    // Cache the label for this snapshot. The clock can expire it, not recalculate it.
    return (account, window, now) => {
      const estimate = estimates.get(trendKey(account, window)), w = estimate?.window
      if (estimate && w && !account.refreshError && now < estimate.at && window.status === w.status && window.used === w.used && window.minutes === w.minutes && window.resetAt === w.resetAt && window.observedAt === w.observedAt) return estimate.label
    }
  }
  private observeWindow(key: string, point: { minutes: number; reset: number; at: number; used: number }) {
    let trend = this.windows.get(key)
    if (!trend) {
      if (this.windows.size >= 128) return
      trend = { minutes: point.minutes, reset: point.reset, change: null, samples: [] }
      this.windows.set(key, trend)
    }
    const previous = trend.samples.at(-1)
    if (previous) {
      if (point.at < previous.at) return
      if (trend.minutes !== point.minutes || trend.reset !== point.reset || point.used < previous.used || point.at - previous.at > 60 || point.at === previous.at && point.used !== previous.used) {
        trend.samples = []; trend.change = null
      } else if (point.at === previous.at) return
      else if (point.used > previous.used) trend.change = point.at
    }
    trend.minutes = point.minutes; trend.reset = point.reset
    trend.samples.push({ at: point.at, used: point.used })
    const cutoff = point.at - (point.minutes >= 10080 ? 1800 : 300)
    while (trend.samples.length > 256 || trend.samples.length > 1 && trend.samples[1].at <= cutoff) trend.samples.shift()
  }
  private deadline(account: Account, window: Window, key: string, now: number): number | undefined {
    if (window.used === null || window.used >= 100 || window.minutes === null || window.resetAt === null) return
    if (window.minutes >= 10080 && account.windows.some((short) => {
      const s = this.sample(short, now)
      return s && ["primary", "secondary"].includes(short.kind) && short.kind !== window.kind && s.minutes < 10080 && s.used >= 100
    })) return
    const f = historicalForecast(account, window, now)
    if (f?.currentStatus === "quota_unchanged" || f?.currentValidUntil != null && now >= f.currentValidUntil) return
    const current = calendarProjection(f, now) ?? f?.current
    const live = current ? current.limitAt : this.localDeadline(key, window, now)
    if (live == null || live <= now || live >= window.resetAt) return
    const usual = f?.usual?.limitAt
    return usual != null && usual > now && usual < window.resetAt ? live + (usual - live) / 2 : live
  }
  private localDeadline(key: string, window: Window, now: number): number | undefined {
    const point = this.sample(window, now), trend = this.windows.get(key)
    if (!point || !trend || trend.minutes !== point.minutes || trend.reset !== point.reset || trend.change === null || now - trend.change >= (point.minutes >= 10080 ? 1800 : 300)) return
    const first = trend.samples[0], last = trend.samples.at(-1)!
    if (point.at !== last.at || point.used !== last.used || last.at - first.at < 60 || last.used <= first.used || point.used >= 100) return
    const seconds = (100 - point.used) / (last.used - first.used) * (last.at - first.at)
    if (Number.isFinite(seconds)) return point.at + seconds
  }
}
