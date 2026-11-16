import type { Account, Snapshot, Window, QuotaForecast } from "./schemas"

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

/** All statistical calculation lives in the runtime; clients only match and format. */
export function historicalForecast(account: Account, window: Window, now: number): QuotaForecast | undefined {
  if (account.refreshError || window.status !== "current" || !fresh(window.observedAt, now)) return
  const f = account.forecasts?.find((f) => f.kind === window.kind && f.minutes === window.minutes && f.resetAt === window.resetAt && f.observedAt === window.observedAt && f.used === window.used && f.resetAt > now)
  if (!f?.calendar) return f
  const s = f.calendar.short
  const matches = s ? account.windows.some((w) => w.kind === s.kind && w.minutes === s.minutes && w.resetAt === s.resetAt && w.observedAt === s.observedAt && w.used === s.used && w.status === "current" && fresh(w.observedAt,now) && w.resetAt !== null && w.resetAt > now) : account.windows.length === 1
  return matches ? f : {...f,calendar:undefined}
}
function estimateDuration(at: number, now: number): string {
  const minutes = Math.max(1, Math.ceil(Math.max(0, at - now) / 60))
  if (minutes < 60) return `~${Math.ceil(minutes / 5) * 5}m`
  if (minutes < 1440) { const rounded = Math.ceil(minutes / 15) * 15; return `~${Math.floor(rounded / 60)}h ${rounded % 60}m` }
  const hours = Math.ceil(minutes / 60)
  return `~${Math.floor(hours / 24)}d ${hours % 24}h`
}
export function currentForecast(f: QuotaForecast | undefined, now: number): string | undefined {
  if (shortBlocked(f,now)) return "Weekly pace paused: short window exhausted"
  if (!f?.current || blockedCurrent(f,now)) return
  const c = calendarProjection(f,now)
  if (c) {
    const suffix = f.calendar!.pauses > 0 ? " incl. short-window pauses" : ""
    return c.limitAt === null ? `At this pace: reset first${suffix}` : `At this pace: ${estimateDuration(c.limitAt,now)} to limit${suffix}`
  }
  if (f.minutes >= 10080) return f.current.limitAt === null ? "Without future pauses: reset first" : `Without future pauses: ${estimateDuration(f.current.limitAt,now)} to limit`
  return f.current.limitAt === null ? "At this pace: reset first" : `At this pace: ${estimateDuration(f.current.limitAt, now)} to limit`
}
function calendarProjection(f: QuotaForecast | undefined, now: number) {
  const c = f?.calendar, s = c?.short
  return c?.validUntil != null && now < c.validUntil && (!s || s.status === "current" && fresh(s.observedAt,now) && s.resetAt !== null && s.resetAt > now) ? c.current : undefined
}
function shortBlocked(f: QuotaForecast | undefined, now: number): boolean {
  const s = f?.calendar?.short
  return !!s && s.minutes !== null && s.minutes > 0 && s.minutes < 10080 && s.used !== null && s.used >= 100 && s.status === "current" && fresh(s.observedAt,now) && s.resetAt !== null && s.resetAt > now
}
function blockedCurrent(f: QuotaForecast | undefined, now: number): string | undefined {
  if (!f) return
  if (f.currentStatus === "quota_unchanged" || f.currentValidUntil != null && now >= f.currentValidUntil) return "Pace unavailable: quota unchanged"
  const p = calendarProjection(f,now) ?? f.current
  if (p?.limitAt != null && now >= p.limitAt) return "Estimate needs fresh quota"
}
export function forecastStatus(f: QuotaForecast | undefined, now: number): string | undefined {
  return blockedCurrent(f, now) ?? ({ insufficient_history: "Building recent pace", reset_changed: "Reset changed; rebuilding pace", reset_time_moving: "Reset time moving; no pace yet" } as Record<string, string>)[f?.currentStatus ?? ""]
}
export function quotaForecastLabel(account: Account, window: Window, now: number, fallback: Forecasts): string | undefined {
  if (account.refreshError || window.status !== "current" || !fresh(window.observedAt, now)) return
  if (window.minutes !== null && window.minutes >= 10080 && window.used !== null && window.used < 100 && window.resetAt !== null && window.resetAt > now && account.windows.some((s) => ["primary","secondary"].includes(s.kind) && s.kind !== window.kind && s.minutes !== null && s.minutes > 0 && s.minutes < 10080 && s.used !== null && s.used >= 100 && s.status === "current" && fresh(s.observedAt,now) && s.resetAt !== null && s.resetAt > now)) return "Weekly pace paused: short window exhausted"
  const f = historicalForecast(account, window, now)
  if (shortBlocked(f,now)) return currentForecast(f,now)
  if (fallback.isIdle(account.id, window, now)) return "Pace unavailable: quota unchanged"
  const local = fallback.label(account.id, window, now)
  return blockedCurrent(f, now) ?? currentForecast(f, now) ?? (local ? window.minutes !== null && window.minutes >= 10080 ? `Without future pauses: ${local}` : `At this pace: ${local}` : undefined) ?? forecastStatus(f, now)
}
export function usualForecast(f: QuotaForecast | undefined, now: number): string | undefined {
  return !f?.usual ? undefined : f.usual.limitAt === null ? "Historical pace: reset first" : f.usual.limitAt <= now ? "Historical estimate needs fresh quota" : `Historical pace: ${estimateDuration(f.usual.limitAt, now)} to limit`
}
export function forecastWarning(f: QuotaForecast | undefined, now: number): string | undefined {
  const beforeReset = f && f.minutes >= 10080 ? calendarProjection(f,now)?.limitAt != null : f?.warning?.limitBeforeReset
  return !f?.warning || blockedCurrent(f, now) || shortBlocked(f,now) ? undefined : `Usage above usual${f.warning.rateRatio === null ? "" : ` · ~${f.warning.rateRatio.toFixed(1)}x normal`}${beforeReset ? " · may run out before reset" : ""}`
}

/** Legacy session-local fallback when the runtime lacks enough recent history. */
export class Forecasts {
  private windows = new Map<string, { minutes: number; reset: number; change: number | null; samples: { at: number; used: number }[] }>()
  private sample(window: Window, now: number) {
    return window.status === "current" && fresh(window.observedAt, now) && window.used !== null && window.minutes !== null && window.minutes > 0 && window.resetAt !== null && window.resetAt > now
      ? { at: window.observedAt, used: window.used, minutes: window.minutes, reset: window.resetAt } : undefined
  }
  isIdle(account: number, window: Window, now: number): boolean {
    const point = this.sample(window, now), trend = this.windows.get(`${account}:${window.kind}`), last = trend?.samples.at(-1)
    if (!point || point.used >= 100 || !trend || !last || trend.minutes !== point.minutes || trend.reset !== point.reset || last.at !== point.at || last.used !== point.used) return false
    const idle = point.minutes >= 10080 ? 1800 : 300
    return trend.change !== null && now - trend.change >= idle || trend.samples[0].used === last.used && last.at - trend.samples[0].at >= idle
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
