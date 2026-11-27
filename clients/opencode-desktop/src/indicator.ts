import type { DashboardState, SessionAccount } from "./schemas"
import { fresh, platformPercent, platformRemaining, visibleWindows } from "./dashboard"
import { duration, percent } from "./format"

/** Match only an observation from this chat's actual primary response. */
export function quotaIndicator(state: DashboardState | undefined, now: number, observation: SessionAccount | null) {
  const report = state?.snapshot
  if (!report || !observation) return
  const account = report.accounts.find((a) => a.observationKey === observation.key)
  if (!account) return
  const cached = state?.error || !fresh(report.checkedAt, now)
  const title = `ExetRouter · ${account.label} · last confirmed account used by this chat`
  if (observation.resource === "platform") {
    const balance = account.platform?.balance
    return {
      id: account.id,
      label: `Platform API ${percent(platformPercent(platformRemaining(balance), balance?.initialUSD), 0)}`,
      title: `${title} via Platform API`,
    }
  }
  const windows = visibleWindows(account)
  const short = windows.find((w) => w.minutes! < 10080), weekly = windows.find((w) => w.minutes === 10080)
  const selected = [short, weekly].filter((w) => w !== undefined)
  const label = selected.map((w) => `${duration(w.minutes)} ${w.status === "current" && fresh(w.observedAt, report.checkedAt) && (w.resetAt === null || w.resetAt > report.checkedAt) ? percent(w.remaining, 0) : "Unknown"}`).join(" · ")
  return { id: account.id, label: label || "ExetRouter", title: `${title}${cached ? "; cached quota snapshot" : ""}` }
}
