import { spawn } from "node:child_process"
import { open } from "node:fs/promises"
import { constants } from "node:fs"
import { homedir } from "node:os"
import { isAbsolute, join } from "node:path"
import type { Options } from "./options"
import type { Snapshot, Usage, UsageInput, HistoryInput, Profile, Tokens, Models, Update, QuotaHistory, PlatformHistory, QuotaForecast } from "./schemas"

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid exr report.")
  return value as Record<string, unknown>
}
function text(value: unknown, max = 512): string {
  if (typeof value !== "string" || value.length > max) throw new Error("Invalid exr report.")
  return value
}
function number(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) throw new Error("Invalid exr report.")
  return value
}
function nullable(value: unknown): number | null { return value == null ? null : number(value) }
function object(value: unknown): Record<string, unknown> { return value == null ? {} : record(value) }
function string(value: unknown, fallback = "Unknown"): string { return value == null ? fallback : text(value) }
function list(value: unknown, max: number): unknown[] {
  if (!Array.isArray(value) || value.length > max) throw new Error("Invalid exr report.")
  return value
}

export function quotaHistory(value: unknown): QuotaHistory | null {
  if (value == null) return null
  const h = record(value), from = number(h.from_utc), to = number(h.to_utc)
  const interval = number(h.interval_seconds)
  if (![600, 3600].includes(interval) || from % interval || to % interval || to <= from || to - from > 2160 * 3600) throw new Error("Invalid quota history.")
  const seen = new Set<string>()
  const points = list(h.points, 2160 * 3600 / interval * 2).map((entry) => {
    const p = record(entry), hour = number(p.hour), observedAt = number(p.observed_at), kind = text(p.kind, 16)
    const key = `${kind}:${hour}`
    if (!["primary", "secondary"].includes(kind) || hour % interval || hour < from || hour >= to || observedAt < hour || observedAt >= hour + interval || seen.has(key)) throw new Error("Invalid quota history.")
    seen.add(key)
    return { kind, hour, observedAt, used: number(p.used_percent), minutes: nullable(p.window_minutes), resetAt: nullable(p.reset_at) }
  }).sort((a, b) => a.hour - b.hour)
  const platformBalance = h.platform_balance == null ? undefined : (() => {
    const b = record(h.platform_balance), enteredAt = number(b.entered_at), initial = number(b.initial_usd_nanos), throughBucket = number(b.through_bucket)
    const first = Math.max(from, Math.floor(enteredAt / interval) * interval)
    if (!Number.isSafeInteger(enteredAt) || !Number.isInteger(initial) || initial <= 0 || initial > 1e16 || throughBucket % interval || throughBucket < first || throughBucket >= to || enteredAt >= to) throw new Error("Invalid Platform balance history.")
    let previousHour = first - interval, previousLeft = 100
    const points = list(b.points, (to - from) / interval).map((entry) => {
      const p = record(entry), hour = number(p.hour), left = number(p.remaining_percent)
      if (hour % interval || hour <= previousHour || hour > throughBucket || left > previousLeft) throw new Error("Invalid Platform balance history.")
      previousHour = hour; previousLeft = left
      return { hour, left }
    })
    if (points[0]?.hour !== first) throw new Error("Invalid Platform balance history.")
    return { enteredAt, initialUSD: initial / 1e9, throughBucket, points }
  })()
  return { from, to, interval, points, ...(h.platform == null ? {} : { platform: platformHistory(h.platform) }), ...(platformBalance ? { platformBalance } : {}) }
}

export function platformHistory(value: unknown): PlatformHistory {
  const h=record(value), from=number(h.from_utc), to=number(h.to_utc)
  if (to<=from || to-from>90*86400 || h.interval_seconds!==86400) throw new Error("Invalid Platform history.")
  const seen=new Set<number>()
  const points=list(h.points,92).map((value)=>{
    const p=record(value), start=number(p.from_utc), end=number(p.to_utc), observedAt=number(p.observed_at), nanos=number(p.usd_nanos)
    if (start%86400 || start<from || end>to || end<=start || end>start+86400 || observedAt<end || seen.has(start) || !Number.isInteger(nanos) || nanos>1e18) throw new Error("Invalid Platform history.")
    seen.add(start)
    return {from:start,to:end,observedAt,usd:nanos/1e9}
  }).sort((a,b)=>a.from-b.from)
  return {from,to,points}
}

function quotaForecasts(value: unknown): QuotaForecast[] {
  const seen = new Set<string>()
  return list(value, 2).map((entry) => {
    const f = record(entry), kind = text(f.kind, 16), minutes = number(f.window_minutes), resetAt = number(f.reset_at), observedAt = number(f.observed_at), used = number(f.used_percent)
    if (!["primary", "secondary"].includes(kind) || seen.has(kind) || !Number.isSafeInteger(minutes) || minutes <= 0 || resetAt <= observedAt || used >= 100) throw new Error("Invalid quota forecast.")
    seen.add(kind)
    const projection = (value: unknown) => {
      if (value == null) return null
      const p = record(value), percentPerHour = number(p.percent_per_hour), limitAt = nullable(p.limit_at)
      if (limitAt !== null && (!Number.isSafeInteger(limitAt) || limitAt <= observedAt || limitAt >= resetAt || percentPerHour <= 0)) throw new Error("Invalid quota forecast.")
      return { percentPerHour, limitAt }
    }
    const current = projection(f.current), usual = projection(f.usual)
    const currentStatus = f.current_status == null ? undefined : text(f.current_status, 32) as NonNullable<QuotaForecast["currentStatus"]>
    const currentValidUntil = f.current_valid_until == null ? undefined : number(f.current_valid_until)
    if (currentStatus !== undefined && !["recent_pace", "insufficient_history", "quota_unchanged", "reset_changed", "reset_time_moving"].includes(currentStatus)
      || currentValidUntil !== undefined && (!Number.isSafeInteger(currentValidUntil) || currentValidUntil > resetAt || current !== null && currentValidUntil <= observedAt)
      || currentStatus !== undefined && (currentStatus === "recent_pace") !== (current !== null)) throw new Error("Invalid quota forecast.")
    const calendar = f.calendar == null ? undefined : (() => {
      const c = record(f.calendar), projected = projection(c.current), validUntil = nullable(c.valid_until), pauses = number(c.pauses)
      const short = c.short == null ? null : (() => {
        const s = record(c.short), sk = text(s.kind,16), sm = nullable(s.window_minutes), sr = nullable(s.reset_at), so = number(s.observed_at), su = number(s.used_percent), remaining = number(s.remaining_percent), status = text(s.status,32)
        if (!["primary","secondary"].includes(sk) || sk === kind || su > 100 || remaining > 100 || Math.abs(remaining-(100-su)) > 1e-9 || sm !== null && (!Number.isSafeInteger(sm) || sm <= 0)) throw new Error("Invalid quota calendar.")
        return {kind:sk, minutes:sm, resetAt:sr, observedAt:so, used:su, remaining, status}
      })()
      if (minutes < 10080 || !Number.isSafeInteger(pauses) || validUntil !== null && (!Number.isSafeInteger(validUntil) || validUntil > resetAt)
        || projected && (!current || projected.percentPerHour <= 0 || validUntil === null || validUntil <= observedAt || currentValidUntil !== undefined && validUntil > currentValidUntil || short && (short.minutes === null || short.minutes >= 10080 || short.observedAt !== observedAt || short.resetAt === null || short.resetAt <= observedAt || short.resetAt-observedAt > short.minutes*60 || validUntil > short.resetAt || short.used >= 100 || short.status !== "current"))
        || !projected && (pauses !== 0 || validUntil !== null) || !short && pauses !== 0) throw new Error("Invalid quota calendar.")
      return {short,current:projected,validUntil,pauses}
    })()
    const warning = f.warning == null ? null : (() => {
      const w = record(f.warning), rateRatio = nullable(w.rate_ratio)
      if (typeof w.limit_before_reset !== "boolean" || !current || !usual || rateRatio !== null && rateRatio <= 0 || w.limit_before_reset !== (current.limitAt !== null)) throw new Error("Invalid quota forecast.")
      return { rateRatio, limitBeforeReset: w.limit_before_reset }
    })()
    const recentSeconds = number(f.recent_seconds), historicalIntervals = number(f.historical_intervals), historicalCycles = number(f.historical_cycles)
    if (![recentSeconds, historicalIntervals, historicalCycles].every(Number.isSafeInteger) || historicalIntervals > 8642 || historicalCycles > historicalIntervals || recentSeconds > 86400 || current && recentSeconds <= 0 || usual && (historicalIntervals < 12 || historicalCycles < 2)) throw new Error("Invalid quota forecast.")
    return { kind, minutes, resetAt, observedAt, used, current, usual, warning, recentSeconds, historicalIntervals, historicalCycles,
      ...(currentStatus === undefined ? {} : { currentStatus }), ...(currentValidUntil === undefined ? {} : { currentValidUntil }), ...(calendar === undefined ? {} : { calendar }) }
  })
}

export function snapshot(value: unknown): Snapshot {
  const root = record(value)
  const report = record("server" in root ? root.server : root)
  if (report.schema_version !== 1) throw new Error("Unsupported exr report.")
  const accounts = report.quota_accounts ?? (report.quota ? [{ label: "Account 1", quota: report.quota }] : [])
  const oauth = object(report.oauth), catalog = object(report.catalog), pool = object(report.pool)
  const limits = object(report.limits)
  const capacity = (value: unknown) => {
    const row = object(value)
    return { active: nullable(row.active_for_user), limit: nullable(row.per_user_limit) }
  }
  return {
    version: text(report.version, 128),
    checkedAt: number(report.checked_at),
    configured: report.configuration_status === "configured",
    routingStatus: report.configuration_status === "configured" ? "Ready" : oauth.active_accounts === 0 ? "No active OAuth accounts"
      : Number(oauth.active_accounts) > 0 && Number(pool.cooldown_accounts) >= Number(oauth.active_accounts) ? "All accounts are paused" : Number(oauth.active_accounts) > 0 && Number(pool.backoff_accounts) >= Number(oauth.active_accounts) ? "Upstream retry delay"
      : catalog.status === "stale" ? "Catalog refresh on next request" : "Setup required; check OAuth and models",
    activeAccounts: nullable(oauth.active_accounts), reauthAccounts: nullable(oauth.reauth_required_accounts),
    models: nullable(catalog.models), catalogStatus: string(catalog.status),
    generations: capacity(limits.generations), websockets: capacity(limits.websockets),
    accounts: list(accounts, 4096).map((entry) => {
      const account = record(entry)
      const quota = record(account.quota)
      const preference = account.preference ? record(account.preference) : undefined
      const routing = object(account.routing), credits = object(account.reset_credits), clients = object(account.clients)
      return {
        platform: account.platform == null ? null : (() => {
          const p = record(account.platform)
          if (!["unknown", "unverified"].includes(p.credit_status as string) || typeof p.inference_enabled !== "boolean") throw new Error("Unsupported Platform status.")
          const costs=p.costs == null ? undefined : (()=>{
            const c=record(p.costs), from=number(c.from_utc), to=number(c.to_utc), nanos=nullable(c.spent_usd_nanos), cents=nullable(c.budget_cents)
            if (to<from || to-from>32*86400 || !["billing_day_utc","calendar_month_utc"].includes(String(c.basis)) || nanos!==null && (!Number.isInteger(nanos)||nanos>1e18) || cents!==null && (!Number.isSafeInteger(cents)||cents>1e9) || c.complete!==true && nanos!==null) throw new Error("Invalid Platform costs.")
            return {from,to,observedAt:nullable(c.observed_at),spentUSD:nanos===null?null:nanos/1e9,budgetUSD:cents===null?null:cents/100,complete:c.complete===true,basis:String(c.basis)}
          })()
          const key=p.key == null ? undefined : (()=>{
            const k=record(p.key), state=text(k.state,32)
            if (!["active","expired","inactive","missing","operation_uncertain"].includes(state)) throw new Error("Invalid Platform key status.")
            return {state,createdAt:nullable(k.created_at),expiresAt:nullable(k.expires_at),lastUsedAt:nullable(k.last_used_at),checkedAt:nullable(k.checked_at)}
          })()
          const balance=p.balance == null ? undefined : (()=>{
            const b=record(p.balance), enteredAt=number(b.entered_at), initial=number(b.initial_usd_nanos)
            if(!Number.isInteger(initial)||initial>1e16) throw new Error("Invalid manual balance.")
            const estimate=(value:unknown)=>{
              const e=record(value), status=text(e.status,32), spent=nullable(e.spent_usd_nanos), through=number(e.through_utc), unpriced=number(e.unpriced_requests)
              const raw=e.remaining_usd_nanos
              if(raw!=null && (typeof raw!=="number" || !Number.isFinite(raw) || !Number.isInteger(raw) || Math.abs(raw)>1e18)) throw new Error("Invalid balance estimate.")
              if(!["estimated","unpriced_requests","configuration_changed","awaiting_report","reported","delayed_report"].includes(status)||!Number.isSafeInteger(unpriced)||spent!==null&&(!Number.isInteger(spent)||spent>1e18)||through<enteredAt) throw new Error("Invalid balance estimate.")
              if(raw!=null&&(spent===null||Math.abs((raw as number)-(initial-spent))>Math.max(1,initial*Number.EPSILON,spent*Number.EPSILON)||unpriced!==0||["configuration_changed","awaiting_report"].includes(status))) throw new Error("Invalid balance estimate.")
              return {remainingUSD:raw==null?null:(raw as number)/1e9,spentUSD:spent===null?null:spent/1e9,through,observedAt:nullable(e.observed_at),unpriced,status}
            }
            return {enteredAt,initialUSD:initial/1e9,exetrouter:estimate(b.exetrouter),organization:estimate(b.organization)}
          })()
          return { configuredAt: number(p.configured_at), inferenceEnabled: p.inference_enabled, creditStatus: "unknown", ...(typeof p.codex_fallback_allowed === "boolean"?{codexFallbackAllowed:p.codex_fallback_allowed}:{}), ...(costs?{costs}:{}), ...(balance?{balance}:{}), ...(key?{key}:{}), ...(p.refresh_error?{refreshError:true}:{}) }
        })(),
        subscription: account.subscription == null ? null : (() => {
          const s = record(account.subscription), day = nullable(s.billing_day), plan = nullable(s.plan_usd), budget = nullable(s.effective_api_budget_cents)
          if (day !== null && (!Number.isInteger(day) || day < 1 || day > 31)) throw new Error("Invalid subscription metadata.")
          if (plan !== null && (!Number.isInteger(plan) || plan < 1 || plan > 100_000)) throw new Error("Invalid subscription metadata.")
          if (budget !== null && (!Number.isSafeInteger(budget) || budget > 1_000_000_000)) throw new Error("Invalid subscription metadata.")
          return { billingDay: day, planUSD: plan, apiBudgetCents: budget }
        })(),
        id: number(account.id ?? 0),
        observationKey: typeof account.observation_key === "string" && /^[a-f0-9]{64}$/.test(account.observation_key) ? account.observation_key : null,
        label: text(account.label),
        enabled: typeof preference?.enabled === "boolean" ? preference.enabled : null,
        locked: preference?.locked === true,
        cooldownUntil: nullable(quota.cooldown_until),
        quotaStatus: string(quota.status), cooldownSource: string(quota.cooldown_source, "Not reported"),
        history: quotaHistory(account.quota_history),
        ...(account.quota_forecast == null ? {} : { forecasts: quotaForecasts(account.quota_forecast) }),
        routing: { reason: string(routing.reason, "unknown"), best: routing.best === true, observedAt: nullable(routing.observed_at), validUntil: nullable(routing.valid_until), deadline: nullable(routing.deadline), targetShare: nullable(routing.target_share_percent) },
        credits: { count: nullable(credits.available_count), observedAt: nullable(credits.observed_at), expiries: list(credits.credits ?? [], 4096).map((entry) => nullable(record(entry).expires_at)).filter((at): at is number => at !== null) },
        automaticReset: string(object(account.automatic_reset).status, "idle"),
        activation: account.weekly_activation == null ? null : text(account.weekly_activation),
        // Do not forward arbitrary native error text across IPC.
        refreshError: typeof account.refresh_error === "string",
        clients: { count: nullable(clients.count), scope: string(clients.scope, "user"), observedAt: nullable(clients.observed_at), truncated: clients.truncated === true,
          rows: list(clients.rows ?? [], 128).map((entry) => {
            const row = record(entry), name = string(row.name, "Label unavailable")
            // Older standalone doctor reports may still contain encrypted labels.
            return { id: string(row.id), name: name.startsWith("exrp1:") ? "Label unavailable" : name, running: number(row.in_flight ?? 0), lastRequestAt: nullable(row.last_request_at) }
          }) },
        windows: list(quota.windows, 16).map((entry) => {
          const window = record(entry)
          const remaining = nullable(window.remaining_percent)
          const used = nullable(window.used_percent)
          if ((remaining !== null && remaining > 100) || (used !== null && used > 100)) throw new Error("Invalid exr report.")
          return {
            kind: text(window.kind, 64),
            remaining,
            used,
            minutes: nullable(window.window_minutes),
            resetAt: nullable(window.reset_at),
            observedAt: number(window.observed_at),
            status: text(window.status, 64),
          }
        }),
      }
    }),
  }
}

export function usage(value: unknown, by: UsageInput["by"]): Usage {
  const report = record(value)
  const row = (entry: unknown) => {
    const r = record(entry)
    return {
      key: text(r.name), name: by === "token" ? string(r.display_name, "Label unavailable") : string(r.display_name ?? r.name),
      requests: number(r.requests), unknown: number(r.unknown_usage), partial: number(r.partial_usage),
      input: nullable(r.input_tokens), output: nullable(r.output_tokens), total: nullable(r.total_tokens), cached: nullable(r.cached_input_tokens),
      usd: nullable(r.estimated_usd), unpriced: number(r.unpriced_requests ?? r.requests),
    }
  }
  return {
    period: text(report.period, 16), timezone: text(report.timezone, 128),
    from: number(report.from_utc), to: number(report.to_utc),
    rows: list(report.rows, 16384).map(row),
    timeline: list(report.timeline ?? [], 744).map((entry) => { const bucket = record(entry); return { from: number(bucket.from_utc), to: number(bucket.to_utc), rows: list(bucket.rows, 16384).map(row) } }),
  }
}

export function tokens(value: unknown): typeof Tokens.Type {
  return list(value, 16384).map((entry) => {
    const row = record(entry)
    const expires = nullable(row.expires_at)
    return { id: text(row.id), name: string(row.name, "Label unavailable"), status: row.revoked_at != null ? "Revoked" : expires !== null && expires <= Date.now() / 1000 ? "Expired" : row.disabled === true ? "Disabled" : "Active",
      fastMode: row.fast_mode === true ? true : row.fast_mode === false ? false : null,
      createdAt: nullable(row.created_at), expiresAt: expires, lastUsedAt: nullable(row.last_used_at) }
  })
}

export function models(value: unknown): typeof Models.Type {
  const rows = list(record(value).data, 16384).map((entry) => {
    const row = record(entry)
    const allowed = ["reasoning", "tools", "image_input", "image_generation", "image_editing", "embeddings"]
    return { id: text(row.id), name: string(row.display_name, "").replaceAll("-", " "), context: nullable(object(row.exetrouter).context_window), source: row.source === "platform" ? "platform" : "codex", capabilities: list(row.capabilities ?? [], 16).map((c) => text(c, 32)).filter((c) => allowed.includes(c)), ...(row.source !== "platform" && row.platform_fallback === true ? { platformFallback: true } : {}) }
  })
  return [...rows.filter((row) => row.source === "codex").reverse(), ...rows.filter((row) => row.source === "platform")]
}

/** Read-only subprocess boundary: no shell, stdin, stderr forwarding, secret output or unbounded buffering. */
export function execJSON(binary: string, args: readonly string[], signal: AbortSignal, allowNonzero = false,
  timeout = 50_000, maxBytes = 8_388_608): Promise<unknown> {
  if (signal.aborted) return Promise.reject(new Error("ExetRouter read cancelled."))
  return new Promise((resolve, reject) => {
    const env: NodeJS.ProcessEnv = { ...process.env, EXR_NO_UPDATE_CHECK: "1" }
    // Dashboard reads use native SSH/local identity, not the inference bearer.
    delete env.EXETROUTER_TOKEN
    const child = spawn(binary, [...args], {
      stdio: ["ignore", "pipe", "ignore"],
      shell: false,
      detached: process.platform !== "win32",
      env,
    })
    let size = 0
    let failed = false
    const chunks: Buffer[] = []
    const kill = () => {
      try {
        if (process.platform !== "win32" && child.pid) process.kill(-child.pid, "SIGKILL")
        else child.kill("SIGKILL")
      } catch { /* The process may already have exited. */ }
    }
    const fail = () => { failed = true; kill() }
    const timer = setTimeout(fail, timeout)
    signal.addEventListener("abort", fail, { once: true })
    // Account for cancellation between the initial check and listener registration.
    if (signal.aborted) fail()
    child.stdout!.on("data", (chunk: Buffer) => {
      size += chunk.length
      if (size > maxBytes) { fail(); return }
      if (!failed) chunks.push(chunk)
    })
    const clean = () => { clearTimeout(timer); signal.removeEventListener("abort", fail) }
    child.once("error", () => { clean(); reject(new Error("Cannot run exr. Check exrBinary in plugin options.")) })
    child.once("close", (code) => {
      clean()
      if (failed || code === null || (code !== 0 && !allowNonzero)) {
        reject(new Error("ExetRouter read failed. Check the saved exr profile and SSH key."))
        return
      }
      try { resolve(JSON.parse(Buffer.concat(chunks).toString("utf8"))) }
      catch { reject(new Error("exr did not return a supported JSON report.")) }
    })
  })
}

function expand(path: string): string { return path.startsWith("~/") ? join(homedir(), path.slice(2)) : path }

export class NativeClient {
  private active = 0
  private available?: Promise<void>
  private resume?: () => void
  constructor(private config: Options, private run = execJSON) {}

  /** One shared slot notification, not a queue of background refresh jobs. */
  whenAvailable(): Promise<void> {
    if (this.active < 2) return Promise.resolve()
    return this.available ??= new Promise((resolve) => { this.resume = resolve })
  }
  private release() {
    this.active--
    if (this.active < 2) {
      const resume = this.resume
      this.resume = undefined; this.available = undefined
      resume?.()
    }
  }

  private async read(args: string[], signal: AbortSignal, allowNonzero = false): Promise<unknown> {
    // Refuse excess work rather than retain an unbounded queue across desktop windows.
    if (this.active >= 2) throw new Error("ExetRouter is busy. Try again after the current read.")
    const profile = this.config.exrConfig && expand(this.config.exrConfig)
    if (profile && !isAbsolute(profile)) throw new Error("exrConfig must be an absolute path or start with ~/.")
    this.active++
    try {
      return await this.run(expand(this.config.exrBinary), [...(profile ? ["--config", profile] : []), ...args, "--json"], signal, allowNonzero)
    } finally { this.release() }
  }

  async snapshot(signal: AbortSignal): Promise<Snapshot> {
    // Never use `limits`: it may submit a weekly activation request. Doctor is passive.
    return snapshot(await this.read(["doctor"], signal, true))
  }

  async usage(input: UsageInput, signal: AbortSignal): Promise<Usage> {
    if (!["24h", "7d", "30d", "90d"].includes(input.period) || !["total", "model", "user", "token"].includes(input.by) || input.before !== undefined && (!Number.isSafeInteger(input.before) || input.before <= 0)) {
      throw new Error("Invalid usage selection.")
    }
    return usage(await this.read(["usage", "--period", input.period, ...(input.by === "total" ? [] : ["--by", input.by]), ...(input.before === undefined ? [] : ["--before", String(input.before)])], signal), input.by)
  }

  async quotaHistory(input: HistoryInput, signal: AbortSignal): Promise<QuotaHistory> {
    if (!Number.isSafeInteger(input.account) || input.account <= 0 || !["24h", "7d", "30d", "90d"].includes(input.period) || input.before !== undefined && (!Number.isSafeInteger(input.before) || input.before <= 0 || input.before % 600 !== 0)) throw new Error("Invalid history selection.")
    const history = quotaHistory(await this.read(["quota-history", String(input.account), "--period", input.period, ...(input.before === undefined ? [] : ["--before", String(input.before)])], signal))
    if (!history) throw new Error("Quota history unavailable.")
    return history
  }

  async tokens(signal: AbortSignal): Promise<typeof Tokens.Type> { return tokens(await this.read(["token", "list"], signal)) }
  async models(signal: AbortSignal): Promise<typeof Models.Type> { return models(await this.read(["models"], signal)) }
  async update(signal: AbortSignal): Promise<typeof Update.Type> {
    if (process.env.EXR_NO_UPDATE_CHECK !== undefined) throw new Error("Update checks are disabled by EXR_NO_UPDATE_CHECK.")
    const report = record(await this.read(["update", "--check"], signal))
    return { current: text(report.current, 128), latest: text(report.latest, 128), available: report.available === true }
  }
  async profile(signal: AbortSignal): Promise<typeof Profile.Type> {
    if (this.active >= 2) throw new Error("ExetRouter is busy. Try again after the current read.")
    if (signal.aborted) throw new Error("ExetRouter read cancelled.")
    this.active++
    let profile: typeof Profile.Type
    try { profile = await this.readProfile(signal) }
    finally { this.release() }
    // Release the file-read slot before taking one for the subprocess. A first
    // dashboard snapshot may be using the other slot at the same time.
    if (profile.mode !== "Standalone") return profile
    const accounts = list(await this.read(["account", "list"], signal), 4096).map((entry) => {
      const row = record(entry)
      return { id: number(row.id), email: string(row.email, "Email unavailable"), state: string(row.state) }
    })
    return { ...profile, accounts }
  }
  private async readProfile(signal: AbortSignal): Promise<typeof Profile.Type> {
    const path = expand(this.config.exrConfig ?? process.env.EXR_CONFIG ?? join(process.env.XDG_CONFIG_HOME ?? join(homedir(), ".config"), "exr/config.json"))
    if (!isAbsolute(path)) throw new Error("Invalid exr profile path.")
    const file = await open(path, constants.O_RDONLY | constants.O_NONBLOCK)
    let root: Record<string, unknown>
    try {
      if (!(await file.stat()).isFile()) throw new Error("Invalid profile.")
      const buffer = Buffer.alloc(65537)
      const { bytesRead } = await file.read(buffer, 0, buffer.length, 0)
      if (bytesRead > 65536 || signal.aborted) throw new Error("Invalid profile.")
      root = record(JSON.parse(buffer.subarray(0, bytesRead).toString("utf8")))
    } finally { await file.close() }
    // Only connection metadata, never arbitrary config fields, keys or credentials.
    const mode = root.mode === "standalone" ? "Standalone" : "Remote Server"
    const destination = mode === "Standalone" ? string(root.listen, "127.0.0.1:8787") : `${string(root.ssh_user, "routercli")}@${string(root.host, "localhost")}:${number(root.port ?? 2222)}`
    const fields = mode === "Standalone"
      ? [{ label: "API endpoint", value: `http://${destination}/v1` }, { label: "Private state", value: string(root.state_dir) }]
      : [{ label: "SSH host", value: string(root.host, "localhost") }, { label: "Port", value: String(number(root.port ?? 2222)) }, { label: "SSH username", value: string(root.ssh_user, "routercli") }, { label: "Private key", value: string(root.identity) }]
    return { mode, destination, fields, accounts: [], connection: null, clientVersion: "Unknown" }
  }
}
