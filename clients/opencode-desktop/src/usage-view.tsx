import { For, Show, createMemo, createSignal } from "solid-js"
import { createLatest } from "@opencode/plugin/gui"
import { activity, activityDate, activityIndex, activityValue, compact, dollars, unpricedRequests } from "./chart"
import { Dashboard } from "./contract"
import { count, date } from "./format"
import type { Usage, UsageInput } from "./schemas"
import { Badge, EmptyState, List, MenuSelect, ReadState, Section, Segments, Stat, useHost } from "./ui"
import { RangeControls, rollingPeriod } from "./time-range"

const groups = [{ value: "total", label: "Total" }, { value: "user", label: "User" }, { value: "model", label: "Model" }, { value: "token", label: "Token" }] as const
const metrics = [{ value: "tokens", label: "Tokens" }, { value: "requests", label: "Requests" }, { value: "usd", label: "USD estimate" }] as const

function ActivityChart(props: { report: Usage; selected: string; metric: "tokens" | "requests" | "usd"; now: number }) {
  const data = createMemo(() => activity(props.report, props.selected, props.metric, props.now))
  const [hover, setHover] = createSignal<number>()
  const index = () => hover() === undefined || !data().buckets.length ? undefined : Math.min(hover()!, data().buckets.length - 1)
  const x = () => 44 + (index() ?? 0) / Math.max(1, data().buckets.length - 1) * 600
  const ticks = createMemo(() => [...new Set([0, 1, 2, 3, 4].map((i) => Math.round(i / 4 * (data().buckets.length - 1))))])
  const label = (at: number) => new Date(at * 1000).toLocaleString("en", props.report.period === "day" || props.report.period === "24h" ? { hour: "2-digit", minute: "2-digit" } : { month: "short", day: "numeric" })
  return <>
    <div class="exr-compact-stats"><Stat label="Requests" value={compact(data().requests)} detail={props.selected ? "Selected group" : "All groups"} />
      <Stat label="Reported tokens" value={<span title={count(data().reported)}>{compact(data().reported)}</span>} detail="Input + output" />
      <Stat label="API equivalent" value={dollars(data().usd)} detail={data().unpriced ? `${count(data().unpriced)} requests not priced` : "Standard USD estimate"} />
    </div>
    <div class="exr-chart exr-activity-chart">
      <Show when={data().buckets.length && data().series.length} fallback={<EmptyState title="No activity yet" description="Activity will appear here when requests are recorded in this period." icon="archive" />}>
         <header class="exr-history-heading"><div><h3>{props.metric === "tokens" ? "Reported tokens" : props.metric === "usd" ? "Standard API equivalent (USD)" : "Requests"}</h3><p class="exr-caption">{props.report.period === "day" || props.report.period === "24h" ? "Hourly" : "Daily"} activity</p></div><div class="exr-history-legend"><For each={data().series}>{(series) => <span><i style={{ background: series.color }} />{series.name}</span>}</For></div></header>
         <div class="exr-chart-plot"><svg viewBox="0 0 668 240" role="img" tabIndex={0} aria-label={`${props.metric === "tokens" ? "Reported token" : props.metric === "usd" ? "Estimated USD" : "Request"} activity over time. Hover or use left and right arrows for interval values.`}
          onPointerMove={(event) => { const rect = event.currentTarget.getBoundingClientRect(); setHover(activityIndex(data().buckets.length, event.clientX, rect.left, rect.width)) }} onPointerLeave={() => setHover(undefined)}
          onFocus={() => setHover(data().buckets.length - 1)} onBlur={() => setHover(undefined)} onKeyDown={(event) => {
            const current = index() ?? data().buckets.length - 1
            const next = event.key === "ArrowLeft" ? Math.max(0, current - 1) : event.key === "ArrowRight" ? Math.min(data().buckets.length - 1, current + 1) : event.key === "Home" ? 0 : event.key === "End" ? data().buckets.length - 1 : event.key === "Escape" ? undefined : null
            if (next === null) return
            event.preventDefault(); setHover(next)
          }}>
           <For each={[0, .25, .5, .75, 1]}>{(ratio) => <g><line x1="44" x2="644" y1={204 - ratio * 184} y2={204 - ratio * 184} class="exr-chart-grid" /><text x="34" y={208 - ratio * 184} text-anchor="end">{props.metric === "usd" ? dollars(data().maximum * ratio) : compact(data().maximum * ratio)}</text></g>}</For>
          <For each={data().series}>{(series) => <>
            <For each={series.segments}>{(points)=><polyline points={points} fill="none" stroke={series.color} stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><title>{series.name}</title></polyline>}</For>
            <For each={series.isolated}>{(p)=><circle cx={p.x} cy={p.y} r="3" fill={series.color} />}</For>
          </>}</For>
          <Show when={index() !== undefined}><g class="exr-chart-cursor" data-slot="activity-cursor"><line x1={x()} x2={x()} y1="20" y2="204" /><For each={data().series}>{(series) => <Show when={series.values[index()!] !== null}><circle cx={x()} cy={204 - series.values[index()!]! / data().maximum * 184} r="3.5" fill={series.color} /></Show>}</For></g></Show>
          <For each={ticks()}>{(i) => <text x={44 + i / Math.max(1, data().buckets.length - 1) * 600} y="231" text-anchor={i === 0 ? "start" : i === data().buckets.length - 1 ? "end" : "middle"}>{label(data().buckets[i].from)}</text>}</For>
        </svg><Show when={index() !== undefined}><div class="exr-chart-tooltip" role="status" style={{ left: x() > 334 ? undefined : `calc(${x() / 668 * 100}% + 24px)`, right: x() > 334 ? `calc(${100 - x() / 668 * 100}% + 24px)` : undefined, "max-width": `calc(${Math.max(x(), 668 - x()) / 668 * 100}% - 32px)` }}>
          <strong>{activityDate(data().buckets[index()!], props.report.period)}</strong>
            <For each={data().series}>{(series) => { const value = () => activityValue(data().buckets[index()!], series.key, props.metric); return <div><span class="exr-inline-start"><i style={{ background: series.color }} />{series.name}</span><span>{props.metric === "usd" ? dollars(value().value) : `${count(value().value)} ${props.metric === "requests" ? "requests" : "tokens"}`}{value().incomplete && props.metric === "usd" ? " (partial)" : ""}</span></div> }}</For>
        </div></Show></div>
      </Show>
    </div>
  </>
}

export function UsageView(props: { now: number; revision?: string }) {
  const ctx = useHost()
  const preferences = ctx.stores.preferences
  const period = createMemo(() => rollingPeriod(preferences.value.period)), by = createMemo(() => preferences.value.by), metric = createMemo(() => preferences.value.metric)
  const [before, setBefore] = createSignal<number>()
  const setPeriod = (value: UsageInput["period"]) => { setBefore(undefined); preferences.update((draft) => { draft.period = value }) }
  const setBy = (value: UsageInput["by"]) => preferences.update((draft) => { draft.by = value })
  const setMetric = (value: "tokens" | "requests" | "usd") => preferences.update((draft) => { draft.metric = value })
  const [selected, setSelected] = createSignal("")
  const live = createMemo((previous: string) => props.revision ?? previous, "initial")
  const report = createLatest(() => ({ period: period(), before: before(), by: by(), live: live() }), async (input, signal) => ({
    ...await ctx.main(Dashboard).usage({ period: input.period, by: input.by, ...(input.before === undefined ? {} : { before: input.before }) }, { signal }), group: input.by,
  }))
  const selectedKey = () => {
    const rows = report.latest?.rows ?? []
    if (rows.some((r) => r.key === selected())) return selected()
    return rows.length > 8 ? rows[0].key : ""
  }
  return <div class="exr-view">
    <Section title="Usage">
      <RangeControls label="Usage" period={period()} before={before()} end={before() ?? report.latest?.to ?? props.now} now={props.now} loading={report.loading} onPeriod={setPeriod} onBefore={setBefore}>
        <MenuSelect label="Group" value={by()} options={groups} onChange={(value) => { setBy(value); setSelected("") }} />
      </RangeControls>
      <ReadState loading={report.loading} error={report.error} hasData={!!report.latest} noun="usage" />
      <Show when={report.latest}>{(data) => <>
        <div class="exr-chart-toolbar"><Segments label="Activity metric" value={metric()} options={metrics} onChange={setMetric} />
          <Show when={data().rows.length > 1}><MenuSelect label="Series" value={selectedKey()} options={[...(data().rows.length <= 8 ? [{ value: "", label: "All groups" }] : []), ...data().rows.map((r) => ({ value: r.key, label: r.name }))]} onChange={setSelected} /></Show>
        </div>
        <ActivityChart report={data()} selected={selectedKey()} metric={metric()} now={props.now} />
        <Show when={metric() === "usd"}><p class="exr-caption">Estimated with published OpenAI standard API prices per model, input/output and cached tokens; long-context rates apply above 272K input tokens. Not actual ChatGPT subscription spending. Unpriced requests are excluded, not counted as $0.</p></Show>
        <p class="exr-caption">{date(data().from)} – {date(data().to)} · {data().timezone}</p>
      </>}</Show>
    </Section>
    <Show when={report.latest}>{(data) => <Section title="Breakdown" description={data().group === "token" ? "Only your API tokens are included." : undefined}>
      <List><For each={data().rows} fallback={<EmptyState title="No requests in this period" description="Try a wider time period to see earlier activity." />}>
        {(row) => <article class="exr-usage-row">
          <div class="exr-inline"><strong class="exr-wrap">{row.name}</strong><Badge>{count(row.requests)} requests</Badge></div>
           <div class="exr-counter-grid"><div><span>Input</span><strong>{count(row.input)}</strong></div><div><span>Output</span><strong>{count(row.output)}</strong></div><div><span>Cached input</span><strong>{count(row.cached)}</strong></div><div><span>Total</span><strong>{count(row.total)}</strong></div><div><span>API equivalent</span><strong>{dollars(row.usd)}{row.usd != null && unpricedRequests(row) ? " (partial)" : ""}</strong></div></div>
        </article>}
      </For></List>
    </Section>}</Show>
  </div>
}
