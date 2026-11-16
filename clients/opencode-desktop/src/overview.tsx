import { For, Show, type Accessor } from "solid-js"
import type { Context } from "@opencode/plugin/gui"
import { best, fresh, priority, routingFresh, routingReason, visibleWindows, historicalForecast, currentForecast, usualForecast, forecastWarning, forecastStatus, quotaForecastLabel, type Forecasts } from "./dashboard"
import { count, date, dollars, duration, freshness, lastUsed, percent, requestAge, shortDate, subscriptionPlan } from "./format"
import type { Account, DashboardState, Window, QuotaForecast } from "./schemas"
import { Badge, EmptyState, Fields, List, Notice, Row, Section, useDashboardState, useDialog, useHost } from "./ui"
import { QuotaHistoryView } from "./quota-history-view"

function AccountDetails(props: { account: Account; now: number; historical?: boolean; forecasts?: Forecasts }) {
  const a = () => props.account
  return <div class="exr exr-dialog-content">
    <div class="exr-detail-quotas"><For each={visibleWindows(a())}>{(window) => <div class="exr-detail-quota"><Quota window={window} historical={!!props.historical || props.account.refreshError || freshness(window, props.now) !== "current"} forecast={props.forecasts ? quotaForecastLabel(a(), window, props.now, props.forecasts) : undefined} outlook={historicalForecast(a(), window, props.now)} now={props.now} /></div>}</For><Show when={a().platform}>{(platform)=><div class="exr-detail-quota exr-platform"><PlatformStatus platform={platform()} now={props.now} details /></div>}</Show></div>
    <QuotaHistoryView account={a()} />
    <Show when={a().subscription}>{(subscription) => <Section title="Subscription & billing">
      <Fields rows={[
        ["Subscription price", subscription().planUSD === null ? "Unknown" : `${dollars(subscription().planUSD)}/month`],
        ["Billing day", subscription().billingDay === null ? "Unknown" : `${subscription().billingDay} of every month`],
        ["Billing source", "Set by the server operator"],
      ]} />
      <p class="exr-caption">The expected billing day is separate from Codex quota resets. It does not confirm a payment or API credit grant.</p>
    </Section>}</Show>
    <Section title={`Clients · ${count(a().clients.count)}`} description={a().clients.scope === "server" ? "Recently active tokens across all users" : "Your recently active tokens"}>
      <Show when={!fresh(a().clients.observedAt, props.now)}><Notice tone="warning">This activity snapshot is historical.</Notice></Show>
      <Show when={a().clients.count === null}><p class="exr-caption">Client activity is not available on this server.</p></Show>
      <Show when={a().clients.count === 0}><p class="exr-caption">No recent inference requests.</p></Show>
      <List><For each={a().clients.rows}>{(client) => <Row title={client.name} description={<><code>{client.id}</code><span>Last request {requestAge(client.lastRequestAt, props.now)} · {date(client.lastRequestAt)}</span></>}>
        <Badge tone={client.running > 0 ? "success" : "neutral"}>{client.running > 0 ? `${count(client.running)} running` : "Idle"}</Badge>
      </Row>}</For></List>
      <Show when={a().clients.truncated}><p class="exr-caption">Showing up to 128 tokens, running requests first.</p></Show>
      <p class="exr-caption">Tokens disappear after 15 minutes without requests; running requests stay visible. One token may represent multiple apps or appear on multiple accounts.</p>
    </Section>
  </div>
}

export function openAccount(ctx: Context, open: Context["dialogs"]["open"], id: number, now: Accessor<number>, state: Accessor<DashboardState | undefined>, forecasts?: Forecasts) {
  const ui = ctx.ui
  open((dialog) => {
    const account = () => state()?.snapshot?.accounts.find((a) => a.id === id)
    return <ui.Dialog title={account()?.label ?? "Account details"} description="Subscription history and account details" size="large"
      footer={<ui.Button onClick={dialog.close}>Done</ui.Button>}>
      <Show when={account()} fallback={<div class="exr exr-dialog-content"><Notice>This account is no longer in the current snapshot.</Notice></div>}>{(a) => <AccountDetails account={a()} now={now()} historical={!!state()?.error} forecasts={forecasts} />}</Show>
    </ui.Dialog>
  })
}

function Quota(props: { window: Window; historical: boolean; forecast?: string; outlook?: QuotaForecast; now: number }) {
  const ui = useHost().ui
  const remaining = () => props.window.remaining
  const tone = () => remaining() === null || props.historical ? "neutral" : remaining()! <= 20 ? "danger" : remaining()! <= 50 ? "warning" : "success"
  return <div class="exr-quota" data-tone={tone()}>
    <div class="exr-inline"><span>{duration(props.window.minutes)}</span><strong>{percent(remaining())}<span class="exr-caption"> left</span></strong></div>
    <div class="exr-quota-track" role={remaining() === null ? "img" : "meter"} aria-label={`${duration(props.window.minutes)} quota remaining${remaining() === null ? ": unknown" : ""}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={remaining() ?? undefined}>
      <Show when={remaining() !== null}><span style={{ width: `${remaining()}%` }} /></Show>
    </div>
    <div class="exr-inline exr-caption"><ui.Tooltip value={`Resets ${date(props.window.resetAt)} · observed ${date(props.window.observedAt)}`}><span>Resets {date(props.window.resetAt)}</span></ui.Tooltip>
      <span>{props.historical ? "Historical" : props.forecast ?? currentForecast(props.outlook, props.now) ?? forecastStatus(props.outlook, props.now)}</span></div>
    <Show when={!props.historical && usualForecast(props.outlook, props.now)}>{(label) => <p class="exr-caption">{label()}</p>}</Show>
    <Show when={!props.historical && forecastWarning(props.outlook, props.now)}>{(label) => <Notice tone="warning">{label()}</Notice>}</Show>
    <Show when={!props.historical && props.outlook?.usual}><p class="exr-caption" title="Observed consumption over the previous 30 days, excluding the most recent day. Reset timestamps are not proof of independent cycles. Missing and exhausted intervals are excluded. These conditional estimates do not predict future workload or paid subscription expiry.">History-based estimate · {props.outlook!.historicalCycles} reported reset times</p></Show>
  </div>
}

function PlatformStatus(props:{platform:NonNullable<Account["platform"]>;now:number;details?:boolean}) {
  const costs=()=>props.platform.costs
  const balance=()=>props.platform.balance
  const remaining=()=>balance()?.exetrouter.remainingUSD
  const initial=()=>balance()?.initialUSD
  const ratio=()=>remaining()!=null && initial()!=null && initial()!>0 ? Math.max(0,Math.min(100,remaining()!/initial()!*100)):undefined
  const amount=(value:number|null|undefined)=>value==null?"Unknown":dollars(value)
  return <div class="exr-quota" data-tone="neutral"><div class="exr-inline"><span>Platform API · estimated left</span><strong>{amount(remaining())}{initial()!=null?` / ${dollars(initial())}`:""}</strong></div>
    <div class="exr-quota-track" role={ratio()===undefined?"img":"meter"} aria-label="Estimated remaining funds from manual balance minus ExetRouter standard token costs" aria-valuemin={0} aria-valuemax={100} aria-valuenow={ratio()}><Show when={ratio()!==undefined}><span style={{width:`${ratio()}%`}} /></Show></div>
    <div class="exr-inline exr-caption"><span>ExetRouter only</span><span>{!balance()?"Manual balance not set":balance()!.exetrouter.status==="configuration_changed"?"Set a new balance after configuration change":balance()!.exetrouter.unpriced?`${balance()!.exetrouter.unpriced} unpriced requests`:`Since ${date(balance()!.enteredAt)}`}</span></div>
    <Show when={props.details}>
      <Show when={balance()}>{b=><><p class="exr-caption">Manual starting balance {dollars(b().initialUSD)} · {date(b().enteredAt)}. A new entry restarts both estimates; there is no automatic monthly refill.</p>
        <div class="exr-inline"><span>Platform organization · estimated left</span><strong>{amount(b().organization.remainingUSD)} / {dollars(b().initialUSD)}</strong></div>
        <p class="exr-caption">{b().organization.status.replaceAll("_"," ")} · reported through {date(b().organization.through)}{b().organization.observedAt!=null?` · observed ${date(b().organization.observedAt)}`:""}. Includes all organization activity; not added to the ExetRouter estimate.</p></>}</Show>
      <p class="exr-caption">ExetRouter uses standard token prices, excluding external usage, tool fees and tier adjustments. Both figures are estimates, not a live provider balance; expiry and top-ups require a new manual entry.</p>
      <p class="exr-caption">Organization costs: {amount(costs()?.spentUSD)}{costs()?.budgetUSD!=null?` / ${dollars(costs()!.budgetUSD)} reference`:""} · {costs()?.complete?"reported":"incomplete history"} · UTC. API routing: {props.platform.inferenceEnabled?"ready":"not ready"}. Codex fallback policy: {props.platform.codexFallbackAllowed?"allowed":"disabled"}.</p>
      <Show when={props.platform.key}>{key=><><p class="exr-caption">Working key · {key().state.replaceAll("_"," ")}</p><div class="exr-token-dates" title={`Created ${date(key().createdAt)} · Expires ${date(key().expiresAt)} · Used ${date(key().lastUsedAt)}`}>Created {shortDate(key().createdAt)} · Expires {shortDate(key().expiresAt)} · Used {lastUsed(key().lastUsedAt,props.now)}</div></>}</Show>
    </Show>
    <Show when={props.platform.refreshError}><p class="exr-caption">Platform refresh unavailable; showing stored observations.</p></Show>
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
      <Show when={subscriptionPlan(a().subscription?.planUSD)}>{(plan) => <span class="exr-caption exr-account-plan">{plan()}</span>}</Show>
    </div></div><ui.Tooltip value="Account details"><ui.IconButton icon="chevron-right" label={`View ${a().label} details`} onClick={() => openAccount(ctx, open, a().id, () => props.now, state, props.forecasts)} /></ui.Tooltip></header>
    <div class="exr-quota-area" data-paused={a().cooldownUntil !== null ? "" : undefined}><div class="exr-quota-grid"><For each={visibleWindows(a())} fallback={<p class="exr-caption">Limits not reported · {a().quotaStatus}</p>}>{(window) =>
      <Quota window={window} historical={props.historical || a().refreshError || freshness(window, props.now) !== "current"} forecast={quotaForecastLabel(a(), window, props.now, props.forecasts)} outlook={historicalForecast(a(), window, props.now)} now={props.now} />
    }</For></div><Show when={a().cooldownUntil !== null}><ui.Tooltip value={`Cooldown source: ${a().cooldownSource}`}><span class="exr-quota-pause">Paused until {date(a().cooldownUntil)}</span></ui.Tooltip></Show></div>
    <Show when={a().platform}>{platform=><div class="exr-platform"><PlatformStatus platform={platform()} now={props.now} /></div>}</Show>
    <footer class="exr-account-footer">
      <ui.Tooltip value={`Reset credits: ${count(a().credits.count)} available${a().credits.expiries.length ? ` · next expires ${date(a().credits.expiries[0])}` : ""}${creditsStale() ? " · historical" : ""}`}>
        <span class="exr-meta" data-tone={expiresSoon() ? "warning" : "neutral"}><ui.Icon name="outline-reset" size="small" />{a().credits.count === null ? "Resets unavailable" : `${count(a().credits.count)} ${a().credits.count === 1 ? "reset" : "resets"}`}{a().credits.expiries.length ? ` · expires ${date(Math.min(...a().credits.expiries))}` : ""}{creditsStale() ? " · historical" : ""}</span>
      </ui.Tooltip>
      <Show when={a().clients.count !== null}><ui.Tooltip value={fresh(a().clients.observedAt, props.now) ? "Recently active API tokens" : "Historical client activity"}><span class="exr-meta"><span class="exr-status-dot" data-tone={fresh(a().clients.observedAt, props.now) ? "success" : "neutral"} />{count(a().clients.count)} {a().clients.count === 1 ? "client" : "clients"}</span></ui.Tooltip></Show>
    </footer>
    <Show when={a().refreshError}><Notice tone="warning">Live subscription limits unavailable; showing stored observations.</Notice></Show>
    <Show when={!["idle", "reset"].includes(a().automaticReset)}><p class="exr-caption">Auto reset: {a().automaticReset}</p></Show>
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
        <p class="exr-limit-summary">Per-user limit: Generations {count(report().generations.active)} / {count(report().generations.limit)} • WebSockets {count(report().websockets.active)} / {count(report().websockets.limit)}</p>
          <div class="exr-account-list"><For each={report().accounts.map((a) => a.id)} fallback={<EmptyState title="No accounts reported" description="Your router has not returned any account observations yet." />}>
            {(id) => <Show when={report().accounts.find((a) => a.id === id)}>{(account) => <AccountCard account={account()} now={props.now} recommended={best(account(), report().checkedAt)} historical={!!state()?.error} forecasts={props.forecasts} />}</Show>}
          </For></div>
      </>}
    </Show>
  </>
}
