import { record } from "./catalog"
export interface Row {
  label: string
  description?: string
  detail?: string
  command?: string
  args?: unknown[]
  accountId?: number
  tokenId?: string
  children?: Row[]
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
    const status = pref.enabled === false ? "Disabled" : text(quota.status)
    const children: Row[] = (Array.isArray(quota.windows) ? quota.windows : []).map((v) => {
      const w = record(v)
      return {
        label:
          typeof w.window_minutes !== "number"
            ? text(w.kind)
            : w.window_minutes % 1440 === 0
              ? `${count(w.window_minutes / 1440)}d`
              : w.window_minutes % 60 === 0
                ? `${count(w.window_minutes / 60)}h`
                : `${count(w.window_minutes)} min`,
        description: `${typeof w.remaining_percent === "number" ? `${count(w.remaining_percent)}% left` : "Unknown remaining"} · ${text(w.status)}`,
        detail: `Resets: ${date(w.reset_at)} · Observed: ${date(w.observed_at)}`,
      }
    })
    if (a.platform)
      children.push({
        label: "Platform inference",
        description: record(a.platform).inference_enabled === true ? "Enabled" : "Disabled",
      })
    return {
      label: text(a.label, `Account ${i + 1}`),
      ...(typeof a.id === "number" && Number.isSafeInteger(a.id) && a.id > 0 ? { accountId: a.id } : {}),
      description: status,
      children,
      detail: [status, ...children.map((c) => `${c.label}: ${c.description}`)].join(" · "),
    }
  })
}
export function usageRows(body: unknown): Row[] {
  const r = record(body)
  if (!Array.isArray(r.rows)) throw new Error("Invalid usage report.")
  return r.rows.map((v) => {
    const m = record(v)
    return {
      label: text(m.display_name ?? m.name),
      description: `${count(m.requests)} requests`,
      detail: `Input: ${count(m.input_tokens)} · Cached input: ${count(m.cached_input_tokens)} · Output: ${count(m.output_tokens)} · Unknown usage: ${count(m.unknown_usage)}`,
    }
  })
}
export function tokenRows(body: unknown): Row[] {
  if (!Array.isArray(body)) throw new Error("Invalid token report.")
  return body.map((v) => {
    const t = record(v)
    return {
      label: text(t.name, "Unlabelled token"),
      ...(typeof t.id === "string" && t.id.length > 0 && t.id.length <= 256
        ? { tokenId: t.id, command: "exetrouter.manageToken", args: [t.id] }
        : {}),
      description:
        t.revoked_at != null
          ? "Revoked"
          : typeof t.expires_at === "number" && t.expires_at <= Date.now() / 1000
            ? "Expired"
            : t.disabled === true
              ? "Disabled"
              : "Active",
      detail: `${text(t.id)} · Created: ${date(t.created_at)} · Expires: ${date(t.expires_at)} · Last used: ${date(t.last_used_at)}`,
    }
  })
}

export function historyRows(value: unknown): Row[] {
  const h = record(value),
    points = h.points
  if (!Array.isArray(points) || points.length > 25920 || ![600, 3600].includes(Number(h.interval_seconds)))
    throw new Error("Invalid quota history.")
  const entries: { at: number; row: Row }[] = []
  for (const raw of points) {
    const p = record(raw)
    if (typeof p.observed_at !== "number" || !["primary", "secondary"].includes(String(p.kind)))
      throw new Error("Invalid quota point.")
    const used = p.used_percent
    entries.push({
      at: p.observed_at,
      row: {
        label: date(p.observed_at),
        description: `${text(p.kind)} · ${typeof used === "number" && used >= 0 && used <= 100 ? `${count(100 - used)}% left` : "Unknown remaining"}`,
        detail: `Window: ${count(p.window_minutes)} min · Resets: ${date(p.reset_at)}`,
      },
    })
  }
  for (const key of ["platform_balance", "platform"] as const) {
    const pp = record(h[key]).points
    if (!Array.isArray(pp)) continue
    if (pp.length > 12960) throw new Error("Invalid Platform history.")
    for (const raw of pp) {
      const p = record(raw),
        at = key === "platform" ? p.from_utc : p.hour
      if (typeof at !== "number") throw new Error("Invalid Platform observation.")
      entries.push({
        at,
        row: {
          label: date(at),
          description:
            key === "platform"
              ? `Platform spend · ${typeof p.usd_nanos === "number" ? `$${count(p.usd_nanos / 1e9)}` : "Unknown"}`
              : `Platform balance · ${typeof p.remaining_percent === "number" ? `${count(p.remaining_percent)}% left` : "Unknown"}`,
        },
      })
    }
  }
  return entries.sort((a, b) => b.at - a.at).map((e) => e.row)
}
