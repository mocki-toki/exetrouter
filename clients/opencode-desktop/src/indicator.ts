import type { DashboardState, SessionAccount } from "./schemas"
import { fresh, visibleWindows } from "./dashboard"
import { duration, percent } from "./format"

/** Match only an observation from this chat's actual primary response. */
export function quotaIndicator(state: DashboardState | undefined, now: number, observation: SessionAccount | null) {
  const report = state?.snapshot
  if (!report || !observation) return
  const account = report.accounts.find((a) => a.observationKey === observation.key)
  if (!account) return
  const windows = visibleWindows(account)
  const short = windows.find((w) => w.minutes! < 10080), weekly = windows.find((w) => w.minutes === 10080)
  const selected = [short, weekly].filter((w) => w !== undefined)
  const label = selected.map((w) => `${duration(w.minutes)} ${w.status === "current" && fresh(w.observedAt, report.checkedAt) && (w.resetAt === null || w.resetAt > report.checkedAt) ? percent(w.remaining) : "Unknown"}`).join(" · ")
  const cached = state?.error || !fresh(report.checkedAt, now)
  return { id: account.id, label: label || "ExetRouter", title: `ExetRouter · ${account.label} · last confirmed account used by this chat${cached ? "; cached quota snapshot" : ""}` }
}
