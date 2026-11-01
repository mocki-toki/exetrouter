import type { Account, Snapshot, Window } from "./schemas"

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

/** Same bounded, session-local observation algorithm as client/quota_forecast.rs. */
export class Forecasts {
  private windows = new Map<string, { minutes: number; reset: number; change: number | null; samples: { at: number; used: number }[] }>()
  private sample(window: Window, now: number) {
    return window.status === "current" && fresh(window.observedAt, now) && window.used !== null && window.minutes !== null && window.minutes > 0 && window.resetAt !== null && window.resetAt > now
      ? { at: window.observedAt, used: window.used, minutes: window.minutes, reset: window.resetAt } : undefined
  }
  observe(report: Snapshot, now: number) {
    const present = new Set<string>()
    for (const account of report.accounts) {
      if (account.refreshError) continue
      for (const window of account.windows) {
        if (!["primary", "secondary"].includes(window.kind)) continue
        const key = `${account.id}:${window.kind}`
        present.add(key)
        const point = this.sample(window, now)
        if (!point) { this.windows.delete(key); continue }
        let trend = this.windows.get(key)
        if (!trend) {
          if (this.windows.size >= 128) continue
          trend = { minutes: point.minutes, reset: point.reset, change: null, samples: [] }
          this.windows.set(key, trend)
        }
        const previous = trend.samples.at(-1)
        if (previous) {
          if (point.at < previous.at) continue
          if (trend.minutes !== point.minutes || trend.reset !== point.reset || point.used < previous.used || point.at - previous.at > 60 || point.at === previous.at && point.used !== previous.used) {
            trend.samples = []; trend.change = null
          } else if (point.at === previous.at) continue
          else if (point.used > previous.used) trend.change = point.at
        }
        trend.minutes = point.minutes; trend.reset = point.reset
        trend.samples.push({ at: point.at, used: point.used })
        const cutoff = point.at - (point.minutes >= 10080 ? 1800 : 300)
        while (trend.samples.length > 256 || trend.samples.length > 1 && trend.samples[1].at <= cutoff) trend.samples.shift()
      }
    }
    for (const key of this.windows.keys()) if (!present.has(key)) this.windows.delete(key)
  }
  label(account: number, window: Window, now: number): string | undefined {
    const point = this.sample(window, now), trend = this.windows.get(`${account}:${window.kind}`)
    if (!point || !trend || trend.minutes !== point.minutes || trend.reset !== point.reset || trend.change === null || now - trend.change >= (point.minutes >= 10080 ? 1800 : 300)) return
    const first = trend.samples[0], last = trend.samples.at(-1)!
    if (point.at !== last.at || point.used !== last.used || last.at - first.at < 60 || last.used <= first.used || point.used >= 100) return
    const seconds = (100 - point.used) / (last.used - first.used) * (last.at - first.at)
    if (!Number.isFinite(seconds) || seconds >= point.reset - now) return
    const minutes = Math.max(1, Math.ceil(seconds / 60))
    const duration = minutes >= 1440 ? `${Math.floor(minutes / 1440)}d ${Math.floor(minutes % 1440 / 60)}h` : minutes >= 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m` : `${minutes}m`
    return `~${duration} to limit`
  }
}
