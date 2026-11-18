import { Schema } from "effect"

export const CatalogStatus = Schema.Struct({
  state: Schema.Literals(["needs_token", "ready", "stale", "error"]),
  baseURL: Schema.String,
  models: Schema.Number,
  fetchedAt: Schema.NullOr(Schema.Number),
})
export type CatalogStatus = typeof CatalogStatus.Type

export const SessionAccount = Schema.Struct({ key: Schema.String, model: Schema.String, observedAt: Schema.Number })
export type SessionAccount = typeof SessionAccount.Type

export const Window = Schema.Struct({
  kind: Schema.String,
  remaining: Schema.NullOr(Schema.Number),
  used: Schema.NullOr(Schema.Number),
  minutes: Schema.NullOr(Schema.Number),
  resetAt: Schema.NullOr(Schema.Number),
  observedAt: Schema.Number,
  status: Schema.String,
})
export type Window = typeof Window.Type

const QuotaProjection = Schema.Struct({ percentPerHour: Schema.Number, limitAt: Schema.NullOr(Schema.Number) })
export const QuotaForecast = Schema.Struct({
  kind: Schema.String, minutes: Schema.Number, resetAt: Schema.Number, observedAt: Schema.Number, used: Schema.Number,
  current: Schema.NullOr(QuotaProjection), usual: Schema.NullOr(QuotaProjection),
  currentStatus: Schema.optionalKey(Schema.Literals(["recent_pace", "insufficient_history", "quota_unchanged", "reset_changed", "reset_time_moving"])),
  currentValidUntil: Schema.optionalKey(Schema.NullOr(Schema.Number)),
  calendar: Schema.optionalKey(Schema.Struct({ short: Schema.NullOr(Window), current: Schema.NullOr(QuotaProjection), validUntil: Schema.NullOr(Schema.Number), pauses: Schema.Number })),
  warning: Schema.NullOr(Schema.Struct({ rateRatio: Schema.NullOr(Schema.Number), limitBeforeReset: Schema.Boolean })),
  recentSeconds: Schema.Number, historicalIntervals: Schema.Number, historicalCycles: Schema.Number,
})
export type QuotaForecast = typeof QuotaForecast.Type

export const PlatformHistory = Schema.Struct({ from: Schema.Number, to: Schema.Number, points: Schema.Array(Schema.Struct({ from: Schema.Number, to: Schema.Number, observedAt: Schema.Number, usd: Schema.Number })) })
export type PlatformHistory = typeof PlatformHistory.Type
export const PlatformCosts = Schema.Struct({ from: Schema.Number, to: Schema.Number, observedAt: Schema.NullOr(Schema.Number), spentUSD: Schema.NullOr(Schema.Number), budgetUSD: Schema.NullOr(Schema.Number), complete: Schema.Boolean, basis: Schema.String })
export const BalanceEstimate = Schema.Struct({ remainingUSD: Schema.NullOr(Schema.Number), spentUSD: Schema.NullOr(Schema.Number), through: Schema.Number, observedAt: Schema.NullOr(Schema.Number), unpriced: Schema.Number, status: Schema.String })
export const PlatformBalance = Schema.Struct({ enteredAt: Schema.Number, initialUSD: Schema.Number, exetrouter: BalanceEstimate, organization: BalanceEstimate })
export const QuotaHistory = Schema.Struct({
  from: Schema.Number, to: Schema.Number, intervalSeconds: Schema.optionalKey(Schema.Number),
  // Pre-release Platform IPC used this name; accept it during GUI reloads.
  interval: Schema.optionalKey(Schema.Number),
  points: Schema.Array(Schema.Struct({ kind: Schema.String, hour: Schema.Number, observedAt: Schema.Number, used: Schema.Number, minutes: Schema.NullOr(Schema.Number), resetAt: Schema.NullOr(Schema.Number) })),
  platform: Schema.optionalKey(Schema.NullOr(PlatformHistory)),
})
export type QuotaHistory = typeof QuotaHistory.Type

export const ClientActivity = Schema.Struct({
  count: Schema.NullOr(Schema.Number), scope: Schema.String, observedAt: Schema.NullOr(Schema.Number), truncated: Schema.Boolean,
  rows: Schema.Array(Schema.Struct({ id: Schema.String, name: Schema.String, running: Schema.Number, lastRequestAt: Schema.NullOr(Schema.Number) })),
})

export const Account = Schema.Struct({
  platform: Schema.optionalKey(Schema.NullOr(Schema.Struct({ configuredAt: Schema.Number, inferenceEnabled: Schema.Boolean, creditStatus: Schema.String, codexFallbackAllowed:Schema.optionalKey(Schema.Boolean),
    costs: Schema.optionalKey(Schema.NullOr(PlatformCosts)),
    balance: Schema.optionalKey(Schema.NullOr(PlatformBalance)),
    key: Schema.optionalKey(Schema.NullOr(Schema.Struct({ state:Schema.String, createdAt:Schema.NullOr(Schema.Number), expiresAt:Schema.NullOr(Schema.Number), lastUsedAt:Schema.NullOr(Schema.Number), checkedAt:Schema.NullOr(Schema.Number) }))),
    refreshError: Schema.optionalKey(Schema.Boolean),
  }))),
  subscription: Schema.optionalKey(Schema.NullOr(Schema.Struct({ billingDay: Schema.NullOr(Schema.Number), planUSD: Schema.NullOr(Schema.Number), apiBudgetCents: Schema.NullOr(Schema.Number) }))),
  observationKey: Schema.NullOr(Schema.String),
  id: Schema.Number, label: Schema.String, enabled: Schema.NullOr(Schema.Boolean), locked: Schema.Boolean,
  windows: Schema.Array(Window), quotaStatus: Schema.String, cooldownSource: Schema.String, cooldownUntil: Schema.NullOr(Schema.Number),
  history: Schema.NullOr(QuotaHistory),
  forecasts: Schema.optionalKey(Schema.Array(QuotaForecast)),
  routing: Schema.Struct({ reason: Schema.String, best: Schema.Boolean, observedAt: Schema.NullOr(Schema.Number), validUntil: Schema.NullOr(Schema.Number), deadline: Schema.NullOr(Schema.Number), targetShare: Schema.NullOr(Schema.Number) }),
  credits: Schema.Struct({ count: Schema.NullOr(Schema.Number), observedAt: Schema.NullOr(Schema.Number), expiries: Schema.Array(Schema.Number) }),
  automaticReset: Schema.String, activation: Schema.NullOr(Schema.String), refreshError: Schema.Boolean,
  clients: ClientActivity,
})
export type Account = typeof Account.Type

export const Snapshot = Schema.Struct({
  version: Schema.String,
  checkedAt: Schema.Number,
  configured: Schema.Boolean,
  routingStatus: Schema.String,
  activeAccounts: Schema.NullOr(Schema.Number), reauthAccounts: Schema.NullOr(Schema.Number),
  models: Schema.NullOr(Schema.Number), catalogStatus: Schema.String,
  generations: Schema.Struct({ active: Schema.NullOr(Schema.Number), limit: Schema.NullOr(Schema.Number) }),
  websockets: Schema.Struct({ active: Schema.NullOr(Schema.Number), limit: Schema.NullOr(Schema.Number) }),
  accounts: Schema.Array(Account),
})
export type Snapshot = typeof Snapshot.Type

export const Update = Schema.Struct({ current: Schema.String, latest: Schema.String, available: Schema.Boolean })
export const DashboardState = Schema.Struct({
  revision: Schema.Number,
  snapshot: Schema.NullOr(Snapshot),
  refreshing: Schema.Boolean,
  error: Schema.NullOr(Schema.String),
  update: Schema.NullOr(Update),
  clipboardPending: Schema.Boolean,
})
export type DashboardState = typeof DashboardState.Type

export const UsageInput = Schema.Struct({
  period: Schema.Literals(["24h", "7d", "30d", "90d"]),
  by: Schema.Literals(["total", "model", "user", "token"]),
  before: Schema.optionalKey(Schema.Number),
})
export type UsageInput = typeof UsageInput.Type
export const HistoryInput = Schema.Struct({ account: Schema.Number, period: Schema.Literals(["24h", "7d", "30d", "90d"]), before: Schema.optionalKey(Schema.Number) })
export type HistoryInput = typeof HistoryInput.Type

export const UsageRow = Schema.Struct({
  key: Schema.String, name: Schema.String, requests: Schema.Number, unknown: Schema.Number, partial: Schema.Number,
  input: Schema.NullOr(Schema.Number), output: Schema.NullOr(Schema.Number), total: Schema.NullOr(Schema.Number), cached: Schema.NullOr(Schema.Number),
  usd: Schema.optionalKey(Schema.NullOr(Schema.Number)), unpriced: Schema.optionalKey(Schema.Number),
})
export const Usage = Schema.Struct({
  period: Schema.String,
  timezone: Schema.String,
  from: Schema.Number,
  to: Schema.Number,
  rows: Schema.Array(UsageRow),
  timeline: Schema.Array(Schema.Struct({ from: Schema.Number, to: Schema.Number, rows: Schema.Array(UsageRow) })),
})
export type Usage = typeof Usage.Type

export const Tokens = Schema.Array(Schema.Struct({
  id: Schema.String, name: Schema.String, status: Schema.String,
  fastMode: Schema.NullOr(Schema.Boolean),
  createdAt: Schema.NullOr(Schema.Number), expiresAt: Schema.NullOr(Schema.Number), lastUsedAt: Schema.NullOr(Schema.Number),
}))
export const Models = Schema.Array(Schema.Struct({ id: Schema.String, name: Schema.String, context: Schema.NullOr(Schema.Number), source: Schema.optionalKey(Schema.String), capabilities: Schema.optionalKey(Schema.Array(Schema.String)) }))
export const Connection = Schema.Struct({
  mode: Schema.Literals(["remote", "standalone"]), host: Schema.String, port: Schema.Number, ssh_user: Schema.String,
  identity: Schema.String, api_url: Schema.NullOr(Schema.String), state_dir: Schema.String, listen: Schema.String,
})
export const Profile = Schema.Struct({
  mode: Schema.String, destination: Schema.String, fields: Schema.Array(Schema.Struct({ label: Schema.String, value: Schema.String })),
  accounts: Schema.Array(Schema.Struct({ id: Schema.Number, email: Schema.String, state: Schema.String })),
  connection: Schema.NullOr(Connection), clientVersion: Schema.String,
})

const action = <T extends string>(name: T) => Schema.Struct({ action: Schema.Literal(name) })
export const DesktopAction = Schema.Union([
  action("profile"), action("copy_retry"), action("copy_discard"), action("login_status"), action("login_cancel"),
  Schema.Struct({ action: Schema.Literal("token_create"), name: Schema.String, days: Schema.Number }),
  Schema.Struct({ action: Schema.Literal("token_rename"), id: Schema.String, name: Schema.String }),
  Schema.Struct({ action: Schema.Literals(["token_rotate", "token_revoke"]), id: Schema.String, confirmed: Schema.Boolean }),
  Schema.Struct({ action: Schema.Literal("token_set_enabled"), id: Schema.String, enabled: Schema.Boolean, confirmed: Schema.Boolean }),
  Schema.Struct({ action: Schema.Literal("token_fast_mode"), id: Schema.String, enabled: Schema.NullOr(Schema.Boolean), confirmed: Schema.Boolean }),
  Schema.Struct({ action: Schema.Literal("configure"), connection: Connection, confirmed: Schema.Boolean }),
  Schema.Struct({ action: Schema.Literal("login_start"), account: Schema.NullOr(Schema.Number) }),
  Schema.Struct({ action: Schema.Literal("account_disable"), account: Schema.Number, confirmed: Schema.Boolean }),
  Schema.Struct({ action: Schema.Literal("update_install"), confirmed: Schema.Boolean }),
])
export type DesktopAction = typeof DesktopAction.Type
export const DesktopResult = Schema.Struct({
  status: Schema.String, copied: Schema.Boolean, retryCopy: Schema.Boolean,
  login: Schema.NullOr(Schema.Struct({ url: Schema.String, code: Schema.String })),
  profile: Schema.NullOr(Profile), update: Schema.NullOr(Update),
})
export type DesktopResult = typeof DesktopResult.Type
