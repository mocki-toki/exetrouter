import { record } from "./catalog"

export interface QuotaMeter {
  label: string
  remaining: number | null
  caption: string
  historical?: boolean
  hint?: string
}

export interface Row {
  label: string
  description?: string
  detail?: string
  command?: string
  args?: unknown[]
  accountId?: number
  tokenId?: string
  tokenState?: string
  fastMode?: boolean | null
  children?: Row[]
  meters?: QuotaMeter[]
}
export function statusSummary(accounts: Row[]) {
  const counts = new Map<string, number>()
  for (const a of accounts) {
    const state = a.description || "Unknown"
    counts.set(state, (counts.get(state) ?? 0) + 1)
  }
  const statuses = [...counts].map(([s, n]) => `${n} ${s}`)
  return statuses.length > 2
    ? `${accounts.length} accounts · ${statuses.length} states`
    : statuses.join(" · ") || "0 accounts"
}
const text = (v: unknown, fallback = "Unknown") =>
  typeof v === "string" && v.length ? v.replace(/[\u0000-\u001f\u007f]/g, " ").slice(0, 512) : fallback
const count = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v.toLocaleString() : "Unknown")
const date = (v: unknown) =>
  typeof v === "number" && Number.isFinite(v) ? new Date(v * 1000).toLocaleString() : "Unknown"
const finite = (v: unknown) => typeof v === "number" && Number.isFinite(v) ? v : null
const percent = (p: number) => `${p.toLocaleString("en", { maximumFractionDigits: 1 })}% left`

function platformMeter(platform: unknown): QuotaMeter {
  const balance = record(record(platform).balance), estimate = record(balance.exetrouter)
  const initial = finite(balance.initial_usd_nanos), spent = finite(estimate.spent_usd_nanos)
  const unpriced = finite(estimate.unpriced_requests)
  const partial = estimate.status === "unpriced_requests" && unpriced !== null && unpriced > 0 && initial !== null && spent !== null
  const left = finite(estimate.remaining_usd_nanos) ?? (partial ? initial - spent : null)
  const dollars = (nanos: number) => {
    const usd = nanos / 1e9
    return usd.toLocaleString("en", { style: "currency", currency: "USD",
      notation: Math.abs(usd) >= 1000 ? "compact" : "standard",
      minimumFractionDigits: 0, maximumFractionDigits: Math.abs(usd) >= 1 ? 2 : 9 })
  }
  return {
    label: partial ? "Platform API · partial estimate" : "Platform API",
    remaining: left !== null && initial !== null && initial > 0 ? Math.max(0, Math.min(100, left / initial * 100)) : null,
    caption: `${left === null ? "Unknown" : dollars(left)}${initial === null ? "" : ` / ${dollars(initial)}`}`,
    hint: partial ? `${unpriced} unpriced ${unpriced === 1 ? "request" : "requests"} excluded` : "Estimated funds remaining through ExetRouter",
  }
}

export function routerRow(body: unknown): Row {
  const root = record(body),
    r = record(root.server ?? root),
    c = record(r.catalog)
  return {
    label: "Router",
    description: text(r.configuration_status),
    children: [
      { label: "Version", description: text(r.version) },
      { label: "Checked", description: date(r.checked_at) },
      { label: "Catalog", description: `${text(c.status)} · ${count(c.models)} models` },
    ],
  }
}
export function accountRows(body: unknown): Row[] {
  const root = record(body),
    report = record(root.server ?? root)
  if (report.schema_version !== 1) throw new Error("Unsupported doctor report.")
  const accounts =
    report.quota_accounts ?? (report.quota ? [{ label: "Account 1", quota: report.quota }] : [])
  if (!Array.isArray(accounts)) throw new Error("Invalid doctor report.")
  return accounts.map((entry, i) => {
    const a = record(entry),
      quota = record(a.quota),
      pref = record(a.preference)
    const id = typeof a.id === "number" && Number.isSafeInteger(a.id) && a.id > 0 ? a.id : undefined
    const status = pref.enabled === false ? "Disabled" : text(quota.status)
    const meters: QuotaMeter[] = []
    const history = id === undefined ? {} : { command: "exetrouter.history", args: [id] }
    const children: Row[] = (Array.isArray(quota.windows) ? quota.windows : []).map((v) => {
      const w = record(v)
      const minutes = finite(w.window_minutes), remaining = finite(w.remaining_percent)
      const label = minutes === null ? text(w.kind) : minutes === 10080 ? "Weekly"
        : minutes % 60 === 0 ? `${count(minutes / 60)}h` : `${count(minutes)} min`
      const reset = date(w.reset_at), resetAt = finite(w.reset_at), checkedAt = finite(report.checked_at)
      const historical = typeof a.refresh_error === "string" || w.status !== "current" ||
        (resetAt !== null && checkedAt !== null && resetAt <= checkedAt)
      if (minutes !== null && minutes > 0) meters.push({
        label, remaining: remaining !== null && remaining >= 0 && remaining <= 100 ? remaining : null,
        caption: `Resets ${reset}`, historical,
      })
      return {
        label,
        description: `${remaining === null ? "Unknown remaining" : percent(remaining)} · ${text(w.status)}`,
        detail: `Resets: ${reset} · Observed: ${date(w.observed_at)}`,
        ...history,
      }
    })
    if (a.platform && typeof a.platform === "object" && !Array.isArray(a.platform)) {
      const meter = platformMeter(a.platform)
      meters.push(meter)
      children.push({
        label: meter.label,
        description: meter.remaining === null ? "Unknown remaining" : percent(meter.remaining),
        detail: `${meter.caption} · ${meter.hint}`,
        ...history,
      })
    }
    return {
      label: text(a.label, `Account ${i + 1}`),
      accountId: id,
      description: status,
      children,
      meters,
      detail: [status, ...children.map((c) => `${c.label}: ${c.description}`)].join(" · "),
    }
  })
}
export function tokenRows(body: unknown): Row[] {
  if (!Array.isArray(body)) throw new Error("Invalid token report.")
  return body.map((v) => {
    const t = record(v)
    const state = t.revoked_at != null ? "Revoked"
      : typeof t.expires_at === "number" && t.expires_at <= Date.now() / 1000 ? "Expired"
      : t.disabled === true ? "Disabled" : "Active"
    const fastMode = typeof t.fast_mode === "boolean" ? t.fast_mode : null
    return {
      label: text(t.name, "Unlabelled token"),
      ...(typeof t.id === "string" && t.id.length > 0 && t.id.length <= 256
        ? { tokenId: t.id, command: "exetrouter.manageToken", args: [t.id] }
        : {}),
      tokenState: state,
      fastMode,
      description: `${state}${fastMode === null ? "" : fastMode ? " · Fast allowed" : " · Fast blocked"}`,
      detail: `${text(t.id)} · Fast: ${fastMode === null ? "Server default" : fastMode ? "Allow" : "Block"} · Created: ${date(t.created_at)} · Expires: ${date(t.expires_at)} · Last used: ${date(t.last_used_at)}`,
    }
  })
}
