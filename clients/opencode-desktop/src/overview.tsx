import { For, Show, type Accessor } from "solid-js"
import type { Context } from "@opencode/plugin/gui"
import { best, fresh, priority, routingFresh, routingReason, visibleWindows, type Forecasts } from "./dashboard"
import { count, date, duration, freshness, percent } from "./format"
import type { Account, DashboardState, Window } from "./schemas"
import { Badge, EmptyState, Fields, List, Notice, Row, Section, Stat, useDashboardState, useDialog, useHost } from "./ui"
import { QuotaHistoryView } from "./quota-history-view"

function AccountDetails(props: { account: Account; now: number }) {
  const a = () => props.account
  return <div class="exr exr-dialog-content">
    <div class="exr-detail-quotas"><For each={visibleWindows(a())}>{(window) => <div class="exr-detail-quota"><Quota window={window} historical={props.account.refreshError || freshness(window, props.now) !== "current"} /></div>}</For></div>
    <QuotaHistoryView account={a()} />
    <Section title="Routing">
      <Fields rows={[
        ["Selection", <>{routingReason(a())}{!routingFresh(a(), props.now) ? " · historical" : ""}</>],
        ["Target share", routingFresh(a(), props.now) && a().routing.targetShare !== null ? `${percent(a().routing.targetShare)} · approximate` : "Unknown"],
        ["Priority deadline", date(a().routing.deadline)],
        ["Availability", a().enabled === null ? "Unknown" : a().enabled ? "Enabled" : "Deactivated"],
        ["Operator lock", a().locked ? "Locked" : "Not locked"],
      ]} />
      <p class="exr-caption">A recommendation for new dialogs, not the account used by this session. Model support and existing affinity can change selection.</p>
    </Section>
    <Section title="Subscription windows">
      <For each={visibleWindows(a())} fallback={<p class="exr-caption">No reported subscription windows.</p>}>{(window) => <div class="exr-detail-group">
        <h4>{duration(window.minutes)}</h4>
        <Fields rows={[["Remaining", percent(window.remaining)], ["Used", percent(window.used)], ["Resets", date(window.resetAt)], ["Observed", date(window.observedAt)], ["Observation", freshness(window, props.now)]]} />
      </div>}</For>
      <Fields rows={[["Limits status", a().quotaStatus], ["Reset credits", count(a().credits.count)], ["Next credit expiry", date(a().credits.expiries[0])], ["Credits observed", date(a().credits.observedAt)], ["Automatic reset", a().automaticReset], ["Weekly activation", a().activation ?? "Not reported"], ["Paused until", a().cooldownUntil === null ? "Not paused" : date(a().cooldownUntil)], ["Cooldown source", a().cooldownSource]]} />
    </Section>
    <Section title={`Clients · ${count(a().clients.count)}`} description={a().clients.scope === "server" ? "Recently active tokens across all users" : "Your recently active tokens"}>
      <Show when={!fresh(a().clients.observedAt, props.now)}><Notice tone="warning">This activity snapshot is historical.</Notice></Show>
      <Show when={a().clients.count === null}><p class="exr-caption">Client activity is not available on this server.</p></Show>
      <Show when={a().clients.count === 0}><p class="exr-caption">No recent inference requests.</p></Show>
      <List><For each={a().clients.rows}>{(client) => <Row title={client.name} description={<><code>{client.id}</code><span>Last request {client.lastRequestAt === null || client.lastRequestAt > props.now ? "Unknown" : props.now - client.lastRequestAt < 60 ? `${props.now - client.lastRequestAt}s ago` : `${Math.floor((props.now - client.lastRequestAt) / 60)}m ago`} · {date(client.lastRequestAt)}</span></>}>
        <Badge tone={client.running > 0 ? "success" : "neutral"}>{client.running > 0 ? `${count(client.running)} running` : "Idle"}</Badge>
      </Row>}</For></List>
      <Show when={a().clients.truncated}><p class="exr-caption">Showing up to 128 tokens, running requests first.</p></Show>
      <p class="exr-caption">Tokens disappear after 15 minutes without requests; running requests stay visible. One token may represent multiple apps or appear on multiple accounts. Target shares use percentages, not calibrated capacity across plans.</p>
    </Section>
  </div>
}

export function openAccount(ctx: Context, open: Context["dialogs"]["open"], id: number, now: Accessor<number>, state: Accessor<DashboardState | undefined>) {
  const ui = ctx.ui
  open((dialog) => {
    const account = () => state()?.snapshot?.accounts.find((a) => a.id === id)
    return <ui.Dialog title={account()?.label ?? "Account details"} description="Subscription and routing details" size="large"
      footer={<ui.Button onClick={dialog.close}>Done</ui.Button>}>
      <Show when={account()} fallback={<div class="exr exr-dialog-content"><Notice>This account is no longer in the current snapshot.</Notice></div>}>{(a) => <AccountDetails account={a()} now={now()} />}</Show>
    </ui.Dialog>
  })
}

function Quota(props: { window: Window; historical: boolean; forecast?: string }) {
  const ui = useHost().ui
  const remaining = () => props.window.remaining
  const tone = () => remaining() === null || props.historical ? "neutral" : remaining()! <= 20 ? "danger" : remaining()! <= 50 ? "warning" : "success"
  return <div class="exr-quota" data-tone={tone()}>
    <div class="exr-inline"><span>{duration(props.window.minutes)}</span><strong>{percent(remaining())}<span class="exr-caption"> left</span></strong></div>
    <div class="exr-quota-track" role={remaining() === null ? "img" : "meter"} aria-label={`${duration(props.window.minutes)} quota remaining${remaining() === null ? ": unknown" : ""}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={remaining() ?? undefined}>
      <Show when={remaining() !== null}><span style={{ width: `${remaining()}%` }} /></Show>
    </div>
    <div class="exr-inline exr-caption"><ui.Tooltip value={`Resets ${date(props.window.resetAt)} · observed ${date(props.window.observedAt)}`}><span>Resets {date(props.window.resetAt)}</span></ui.Tooltip>
      <span>{props.historical ? "Historical" : props.forecast}</span></div>
  </div>
}

function AccountCard(props: { account: Account; now: number; historical: boolean; recommended: boolean; forecasts: Forecasts }) {
  const ctx = useHost(), ui = ctx.ui, open = useDialog()
  const state = useDashboardState()
  const a = () => props.account
  const creditsStale = () => props.historical || a().refreshError || !fresh(a().credits.observedAt, props.now)
  const expiresSoon = () => (a().credits.count ?? 0) > 0 && a().credits.expiries.some((at) => at <= props.now + 604800)
  return <article class="exr-account" data-disabled={a().enabled === false}>
    <header class="exr-account-header"><div class="exr-row-main"><h4 title={a().label}>{a().label}</h4><div class="exr-inline-start">
      <Show when={a().enabled === false}><Badge>Deactivated</Badge></Show>
      <Show when={props.recommended}><span class="exr-best" title={props.historical || !best(a(), props.now) ? "Last reported Best recommendation · cached snapshot" : "Best candidate for new dialogs"}>Best</span></Show>
      <Show when={priority(a())}><ui.Tooltip value={`${routingReason(a())}${!routingFresh(a(), props.now) ? " · historical" : ""}`}><span><Badge tone="danger">Priority</Badge></span></ui.Tooltip></Show>
      <Show when={a().locked}><ui.Tooltip value="Operator locked"><span class="exr-caption"><ui.Icon name="lock" size="small" /></span></ui.Tooltip></Show>
    </div></div><ui.Tooltip value="Account details"><ui.IconButton icon="chevron-right" label={`View ${a().label} details`} onClick={() => openAccount(ctx, open, a().id, () => props.now, state)} /></ui.Tooltip></header>
    <div class="exr-quota-area" data-paused={a().cooldownUntil !== null ? "" : undefined}><div class="exr-quota-grid"><For each={visibleWindows(a())} fallback={<p class="exr-caption">Limits not reported · {a().quotaStatus}</p>}>{(window) =>
      <Quota window={window} historical={props.historical || a().refreshError || freshness(window, props.now) !== "current"} forecast={props.forecasts.label(a().id, window, props.now)} />
    }</For></div><Show when={a().cooldownUntil !== null}><ui.Tooltip value={`Cooldown source: ${a().cooldownSource}`}><span class="exr-quota-pause">Paused until {date(a().cooldownUntil)}</span></ui.Tooltip></Show></div>
    <footer class="exr-account-footer">
      <ui.Tooltip value={`Reset credits: ${count(a().credits.count)} available${a().credits.expiries.length ? ` · next expires ${date(a().credits.expiries[0])}` : ""}${creditsStale() ? " · historical" : ""}`}>
        <span class="exr-meta" data-tone={expiresSoon() ? "warning" : "neutral"}><ui.Icon name="outline-reset" size="small" />{a().credits.count === null ? "Resets unavailable" : `${count(a().credits.count)} ${a().credits.count === 1 ? "reset" : "resets"}`}{a().credits.expiries.length ? ` · expires ${date(Math.min(...a().credits.expiries))}` : ""}{creditsStale() ? " · historical" : ""}</span>
      </ui.Tooltip>
      <Show when={a().clients.count !== null}><ui.Tooltip value={fresh(a().clients.observedAt, props.now) ? "Recently active API tokens" : "Historical client activity"}><span class="exr-meta"><span class="exr-status-dot" data-tone={fresh(a().clients.observedAt, props.now) ? "success" : "neutral"} />{count(a().clients.count)} {a().clients.count === 1 ? "client" : "clients"}</span></ui.Tooltip></Show>
    </footer>
    <Show when={a().refreshError}><Notice tone="warning">Live subscription limits unavailable; showing stored observations.</Notice></Show>
    <Show when={a().automaticReset !== "idle"}><p class="exr-caption">Auto reset: {a().automaticReset}</p></Show>
    <Show when={a().activation && a().activation !== "completed"}><p class="exr-caption">Weekly activation: {a().activation} · gpt-5.6-sol</p></Show>
  </article>
}

export function Overview(props: { now: number; forecasts: Forecasts }) {
  const ctx = useHost(), ui = ctx.ui
  const state = useDashboardState()
  return <>
    <Show when={state()?.error}><Notice tone="warning">Connection unavailable. The last snapshot, if shown, is historical.</Notice></Show>
    <Show when={state()?.snapshot} fallback={<Show when={!state()?.refreshing} fallback={<div class="exr-loading"><ui.Loader />Loading your router…</div>}><EmptyState icon="server" title="No router snapshot" description="Check your saved exr profile, then refresh to load accounts." /></Show>}>
      {(report) => <>
        <div class="exr-router-status"><span class="exr-status-dot" data-tone={state()?.error || !fresh(report().checkedAt, props.now) ? "neutral" : report().configured ? "success" : "warning"} />
          <strong class="exr-truncate" title={report().routingStatus}>ExetRouter v{report().version.replace(/^v/, "")} {state()?.error ? "· cached snapshot" : report().routingStatus === "Ready" ? "ready" : `· ${report().routingStatus}`}</strong><span class="exr-spacer" />
          <ui.Tooltip value={`Snapshot ${date(report().checkedAt)}. Public API readiness and inference are not checked.`}><span class="exr-caption">{fresh(report().checkedAt, props.now) && !state()?.error ? "Updated just now" : "Cached snapshot"}</span></ui.Tooltip>
        </div>
        <div class="exr-compact-stats"><Stat label="Models" value={count(report().models)} detail={report().catalogStatus === "stale" ? "Cached · refreshes on next request" : report().catalogStatus} />
          <Stat label="Generations" value={<>{count(report().generations.active)}<span class="exr-stat-limit"> / {count(report().generations.limit)}</span></>} detail="Active · per-user limit" />
          <Stat label="WebSockets" value={<>{count(report().websockets.active)}<span class="exr-stat-limit"> / {count(report().websockets.limit)}</span></>} detail="Active · per-user limit" />
        </div>
          <div class="exr-account-list"><For each={report().accounts.map((a) => a.id)} fallback={<EmptyState title="No accounts reported" description="Your router has not returned any account observations yet." />}>
            {(id) => <Show when={report().accounts.find((a) => a.id === id)}>{(account) => <AccountCard account={account()} now={props.now} recommended={best(account(), report().checkedAt)} historical={!!state()?.error} forecasts={props.forecasts} />}</Show>}
          </For></div>
      </>}
    </Show>
  </>
}
