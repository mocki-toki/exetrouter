import type { Window } from "./schemas"

export function count(value: number | null | undefined): string {
  return value == null ? "Unknown" : new Intl.NumberFormat("en").format(value)
}
export function percent(value: number | null): string {
  return value === null ? "Unknown" : `${new Intl.NumberFormat("en", { maximumFractionDigits: 1 }).format(value)}%`
}
export function date(seconds: number | null | undefined): string {
  if (seconds == null) return "Unknown"
  const date = new Date(seconds * 1000)
  return Number.isNaN(date.getTime()) ? "Unknown" : date.toLocaleString("en", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })
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
