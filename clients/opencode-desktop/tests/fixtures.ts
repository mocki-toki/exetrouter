export function catalog() {
  return { providers: { exetrouter: {
    name: "ExetRouter", package: "@opencode/ai/providers/openai/responses",
    models: { "test-reasoner": {
      name: "Synthetic Reasoner", limit: { context: 100000, input: 80000, output: 0 },
      capabilities: { tools: true, input: ["text", "image"], output: ["text"] },
      settings: { reasoningEffort: "low", reasoningSummary: "none", textVerbosity: "low" },
      variants: ["low", "high", "future"].map((id) => ({ id, settings: { reasoningEffort: id } })),
    } },
  } } }
}

export function doctor() {
  return {
    schema_version: 1, version: "0.0.0-test", checked_at: 1800000000, configuration_status: "configured",
    oauth: { active_accounts: 3, reauth_required_accounts: 1 }, catalog: { models: 7, status: "current" },
    limits: { generations: { active_for_user: 2, per_user_limit: 8 }, websockets: { active_for_user: 1, per_user_limit: 4 } },
    quota_accounts: [{ id: 1, observation_key: "a".repeat(64), label: "Synthetic account", preference: { enabled: true, locked: false },
      routing: { best: true, reason: "reset_expiring", observed_at: 1800000000, valid_until: 1800003600, deadline: 1800003600, target_share_percent: 65 },
      reset_credits: { available_count: 2, observed_at: 1800000000, credits: [{ expires_at: 1800003600 }] },
      automatic_reset: { status: "waiting" }, weekly_activation: "pending",
      clients: { count: 1, scope: "server", observed_at: 1800000000, truncated: false, rows: [{ id: "client-alias", name: "Synthetic client", in_flight: 2, last_request_at: 1800000000 }] },
      quota_history: { from_utc: 1800000000 - 167 * 3600, to_utc: 1800003600, interval_seconds: 3600, points: [
        {kind:"primary",hour:1800000000-3600,observed_at:1800000000-3500,used_percent:80,window_minutes:480,reset_at:1800003600},
        {kind:"secondary",hour:1800000000-3600,observed_at:1800000000-3500,used_percent:40,window_minutes:10080,reset_at:1800604800},
        {kind:"primary",hour:1800000000,observed_at:1800000000,used_percent:100,window_minutes:480,reset_at:1800003600},
        {kind:"secondary",hour:1800000000,observed_at:1800000000,used_percent:50,window_minutes:10080,reset_at:1800604800},
      ] },
      quota: {
      status: "current", windows: [{ kind: "primary", remaining_percent: 0, used_percent: 100, window_minutes: 480,
        reset_at: 1800003600, observed_at: 1800000000, status: "current" }],
    } }],
  }
}

export function usage() {
  return { period: "day", timezone: "UTC", from_utc: 1800000000, to_utc: 1800086400, rows: [
    { name: "test-reasoner", requests: 2, unknown_usage: 1, known_usage: 1, partial_usage: 0,
      input_tokens: 0, output_tokens: null, total_tokens: null },
  ], timeline: [{ from_utc: 1800000000, to_utc: 1800003600, rows: [
    { name: "test-reasoner", requests: 2, unknown_usage: 1, partial_usage: 0, input_tokens: 0, output_tokens: null, total_tokens: null },
  ] }] }
}

export function tokenList() { return [{ id: "tok_synthetic", name: "Synthetic token", created_at: 1800000000, expires_at: 1900000000, revoked_at: null, last_used_at: null }] }
export function modelList() { return { data: [{ id: "synthetic-model", display_name: "Synthetic model", exetrouter: { context_window: 100000 } }] } }

export function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: Error) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
