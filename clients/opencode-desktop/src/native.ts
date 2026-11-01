import { spawn } from "node:child_process"
import { open } from "node:fs/promises"
import { constants } from "node:fs"
import { homedir } from "node:os"
import { isAbsolute, join } from "node:path"
import type { Options } from "./options"
import type { Snapshot, Usage, UsageInput, Profile, Tokens, Models, Update, QuotaHistory } from "./schemas"

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
  if (h.interval_seconds !== 3600 || from % 3600 || to % 3600 || to <= from || to - from > 168 * 3600) throw new Error("Invalid quota history.")
  const seen = new Set<string>()
  const points = list(h.points, 336).map((entry) => {
    const p = record(entry), hour = number(p.hour), observedAt = number(p.observed_at), kind = text(p.kind, 16)
    const key = `${kind}:${hour}`
    if (!["primary", "secondary"].includes(kind) || hour % 3600 || hour < from || hour >= to || observedAt < hour || observedAt >= hour + 3600 || seen.has(key)) throw new Error("Invalid quota history.")
    seen.add(key)
    return { kind, hour, observedAt, used: number(p.used_percent), minutes: nullable(p.window_minutes), resetAt: nullable(p.reset_at) }
  }).sort((a, b) => a.hour - b.hour)
  return { from, to, points }
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
        id: number(account.id ?? 0),
        observationKey: typeof account.observation_key === "string" && /^[a-f0-9]{64}$/.test(account.observation_key) ? account.observation_key : null,
        label: text(account.label),
        enabled: typeof preference?.enabled === "boolean" ? preference.enabled : null,
        locked: preference?.locked === true,
        cooldownUntil: nullable(quota.cooldown_until),
        quotaStatus: string(quota.status), cooldownSource: string(quota.cooldown_source, "Not reported"),
        history: quotaHistory(account.quota_history),
        routing: { reason: string(routing.reason, "unknown"), best: routing.best === true, observedAt: nullable(routing.observed_at), validUntil: nullable(routing.valid_until), deadline: nullable(routing.deadline), targetShare: nullable(routing.target_share_percent) },
        credits: { count: nullable(credits.available_count), observedAt: nullable(credits.observed_at), expiries: list(credits.credits ?? [], 4096).map((entry) => nullable(record(entry).expires_at)).filter((at): at is number => at !== null) },
        automaticReset: string(object(account.automatic_reset).status, "idle"),
        activation: account.weekly_activation == null ? null : text(account.weekly_activation),
        // Do not forward arbitrary native error text across IPC.
        refreshError: typeof account.refresh_error === "string",
        clients: { count: nullable(clients.count), scope: string(clients.scope, "user"), observedAt: nullable(clients.observed_at), truncated: clients.truncated === true,
          rows: list(clients.rows ?? [], 128).map((entry) => { const row = record(entry); return { id: string(row.id), name: string(row.name, "Label unavailable"), running: number(row.in_flight ?? 0), lastRequestAt: nullable(row.last_request_at) } }) },
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
    return { id: text(row.id), name: string(row.name, "Label unavailable"), status: row.revoked_at != null ? "Revoked" : expires !== null && expires <= Date.now() / 1000 ? "Expired" : "Active",
      createdAt: nullable(row.created_at), expiresAt: expires, lastUsedAt: nullable(row.last_used_at) }
  })
}

export function models(value: unknown): typeof Models.Type {
  return list(record(value).data, 16384).map((entry) => { const row = record(entry); return { id: text(row.id), name: string(row.display_name, ""), context: nullable(object(row.exetrouter).context_window) } }).reverse()
}

/** Read-only subprocess boundary: no shell, stdin, stderr forwarding, secret output or unbounded buffering. */
export function execJSON(binary: string, args: readonly string[], signal: AbortSignal, allowNonzero = false,
  timeout = 50_000, maxBytes = 1_048_576): Promise<unknown> {
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
    if (!["day", "24h", "week", "month"].includes(input.period) || !["total", "model", "user", "token"].includes(input.by)) {
      throw new Error("Invalid usage selection.")
    }
    return usage(await this.read(["usage", "--period", input.period, ...(input.by === "total" ? [] : ["--by", input.by])], signal), input.by)
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
