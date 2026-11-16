import type { Window } from "./schemas"

export function dollars(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—"
  if (value < 0) return `-${dollars(-value)}`
  const abs = Math.abs(value)
  for (const [scale, suffix] of [[1e12, "T"], [1e9, "B"], [1e6, "M"], [1e3, "k"]] as const) {
    if (abs >= scale - scale / 20_000) return `$${(value / scale).toFixed(1).replace(/\.0$/, "")}${suffix}`
  }
  if (abs > 0 && abs < 1e-8) return "<$0.00000001"
  const places = Number.isInteger(value) ? 0 : abs < .0001 ? 8 : abs < .01 ? 5 : 2
  return `$${value.toLocaleString("en", { minimumFractionDigits: Number.isInteger(value)?0:2, maximumFractionDigits: places })}`
}

export function count(value: number | null | undefined): string {
  return value == null ? "Unknown" : new Intl.NumberFormat("en").format(value)
}
export function subscriptionPlan(price: number | null | undefined): string | undefined {
  if (price == null || !Number.isFinite(price) || price <= 0) return undefined
  if (price === 20) return "Plus $20"
  if (price === 100 || price === 200) return `Pro ${dollars(price)}`
  return `${dollars(price)}/month`
}
export function percent(value: number | null): string {
  return value === null ? "Unknown" : `${new Intl.NumberFormat("en", { maximumFractionDigits: 1 }).format(value)}%`
}
export function date(seconds: number | null | undefined): string {
  if (seconds == null) return "Unknown"
  const date = new Date(seconds * 1000)
  return Number.isNaN(date.getTime()) ? "Unknown" : date.toLocaleString("en", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })
}
export function shortDate(seconds: number | null | undefined): string {
  if (seconds == null) return "Unknown"
  const value = new Date(seconds * 1000)
  return Number.isNaN(value.getTime()) ? "Unknown" : value.toLocaleDateString("en", { month: "short", day: "numeric" })
}
export function requestAge(seconds: number | null, now: number): string {
  if (seconds === null || !Number.isFinite(seconds) || !Number.isFinite(now) || seconds > now) return "Unknown"
  const age = Math.floor(now - seconds)
  if (age < 60) return `${age}s ago`
  if (age < 3600) return `${Math.floor(age / 60)}m ago`
  if (age < 86400) return `${Math.floor(age / 3600)}h ago`
  return `${Math.floor(age / 86400)}d ago`
}
export function lastUsed(seconds: number | null, now: number): string {
  if (seconds === null) return "Never"
  if (seconds > now) return shortDate(seconds)
  const minutes = Math.floor((now - seconds) / 60)
  if (minutes < 1) return "now"
  if (minutes < 60) return `${minutes}m ago`
  if (minutes < 1440) return `${Math.floor(minutes / 60)}h ago`
  return shortDate(seconds)
}
export function duration(minutes: number | null): string {
  if (minutes == null) return "Unknown duration"
  if (minutes === 10080) return "Weekly"
  if (minutes % 60 === 0) return `${minutes / 60}-hour`
  return `${minutes}-minute`
}
export function freshness(window: Window, now = Date.now() / 1000): string {
  if (window.status !== "current") return window.status
  if (window.observedAt > now || now - window.observedAt >= 60 || (window.resetAt !== null && window.resetAt <= now)) return "historical"
  return "current"
}
