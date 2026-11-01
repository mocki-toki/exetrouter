import { For, Show, createMemo, createSignal, createUniqueId } from "solid-js"
import { createLatest } from "@opencode/plugin/gui"
import { activity, activityDate, activityIndex, activityValue, compact } from "./chart"
import { Dashboard } from "./contract"
import { count, date } from "./format"
import type { Usage, UsageInput } from "./schemas"
import { Badge, EmptyState, List, MenuSelect, ReadState, Section, Segments, Stat, useHost } from "./ui"

const periods = [{ value: "day", label: "Today" }, { value: "24h", label: "24 hours" }, { value: "week", label: "Week" }, { value: "month", label: "Month" }] as const
const groups = [{ value: "total", label: "Total" }, { value: "user", label: "User" }, { value: "model", label: "Model" }, { value: "token", label: "Token" }] as const
const metrics = [{ value: "tokens", label: "Tokens" }, { value: "requests", label: "Requests" }] as const

function ActivityChart(props: { report: Usage; selected: string; metric: "tokens" | "requests"; now: number }) {
  const data = createMemo(() => activity(props.report, props.selected, props.metric, props.now))
  const [hover, setHover] = createSignal<number>()
  const index = () => hover() === undefined || !data().buckets.length ? undefined : Math.min(hover()!, data().buckets.length - 1)
  const x = () => 44 + (index() ?? 0) / Math.max(1, data().buckets.length - 1) * 600
  const gradient = createUniqueId()
  const label = (at: number) => new Date(at * 1000).toLocaleString("en", props.report.period === "day" || props.report.period === "24h" ? { hour: "2-digit", minute: "2-digit" } : { month: "short", day: "numeric" })
  return <>
    <div class="exr-compact-stats"><Stat label="Requests" value={compact(data().requests)} detail={props.selected ? "Selected group" : "All groups"} />
      <Stat label="Reported tokens" value={<span title={count(data().reported)}>{compact(data().reported)}</span>} detail="Input + output" />
    </div>
    <div class="exr-chart">
      <Show when={data().buckets.length && data().series.length} fallback={<EmptyState title="No activity yet" description="Activity will appear here when requests are recorded in this period." icon="archive" />}>
        <div class="exr-chart-plot"><svg viewBox="0 0 668 220" role="img" tabIndex={0} aria-label={`${props.metric === "tokens" ? "Reported token" : "Request"} activity over time. Hover or use left and right arrows for interval values.`}
          onPointerMove={(event) => { const rect = event.currentTarget.getBoundingClientRect(); setHover(activityIndex(data().buckets.length, event.clientX, rect.left, rect.width)) }} onPointerLeave={() => setHover(undefined)}
          onFocus={() => setHover(data().buckets.length - 1)} onBlur={() => setHover(undefined)} onKeyDown={(event) => {
            const current = index() ?? data().buckets.length - 1
            const next = event.key === "ArrowLeft" ? Math.max(0, current - 1) : event.key === "ArrowRight" ? Math.min(data().buckets.length - 1, current + 1) : event.key === "Home" ? 0 : event.key === "End" ? data().buckets.length - 1 : event.key === "Escape" ? undefined : null
            if (next === null) return
            event.preventDefault(); setHover(next)
          }}>
          <defs><linearGradient id={gradient} x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stop-color="var(--exr-accent)" stop-opacity=".2" /><stop offset="100%" stop-color="var(--exr-accent)" stop-opacity="0" /></linearGradient></defs>
          <For each={[0, .5, 1]}>{(ratio) => <g><line x1="44" x2="644" y1={178 - ratio * 154} y2={178 - ratio * 154} class="exr-chart-grid" /><text x="34" y={182 - ratio * 154} text-anchor="end">{compact(data().maximum * ratio)}</text></g>}</For>
          <Show when={data().series.length === 1 && data().buckets.length > 1}><polygon points={`44,178 ${data().series[0].points} 644,178`} fill={`url(#${gradient})`} /></Show>
          <For each={data().series}>{(series) => <>
            <polyline points={series.points} fill="none" stroke={series.color} stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke"><title>{series.name}</title></polyline>
            <Show when={data().buckets.length === 1}><circle cx="44" cy={178 - series.values[0] / data().maximum * 154} r="4" fill={series.color} /></Show>
          </>}</For>
          <Show when={index() !== undefined}><g class="exr-chart-cursor" data-slot="activity-cursor"><line x1={x()} x2={x()} y1="24" y2="178" /><For each={data().series}>{(series) => <circle cx={x()} cy={178 - series.values[index()!] / data().maximum * 154} r="3" fill={series.color} />}</For></g></Show>
          <text x="44" y="210">{label(data().buckets[0].from)}</text><text x="644" y="210" text-anchor="end">{label(data().buckets.at(-1)!.from)}</text>
        </svg><Show when={index() !== undefined}><div class="exr-chart-tooltip" role="status" style={{ left: x() > 334 ? undefined : `calc(${x() / 668 * 100}% + 24px)`, right: x() > 334 ? `calc(${100 - x() / 668 * 100}% + 24px)` : undefined, "max-width": `calc(${Math.max(x(), 668 - x()) / 668 * 100}% - 32px)` }}>
          <strong>{activityDate(data().buckets[index()!], props.report.period)}</strong>
          <For each={data().series}>{(series) => { const value = () => activityValue(data().buckets[index()!], series.key, props.metric); return <div><span>{series.name}</span><span>{count(value().value)} {props.metric === "requests" ? "requests" : "tokens"}</span></div> }}</For>
        </div></Show></div>
        <div class="exr-chart-legend"><For each={data().series}>{(series) => <span><i style={{ background: series.color }} />{series.name}</span>}</For></div>
      </Show>
    </div>
  </>
}

export function UsageView(props: { now: number; revision?: string }) {
  const ctx = useHost()
  const preferences = ctx.stores.preferences
  const period = createMemo(() => preferences.value.period), by = createMemo(() => preferences.value.by), metric = createMemo(() => preferences.value.metric)
  const setPeriod = (value: UsageInput["period"]) => preferences.update((draft) => { draft.period = value })
  const setBy = (value: UsageInput["by"]) => preferences.update((draft) => { draft.by = value })
  const setMetric = (value: "tokens" | "requests") => preferences.update((draft) => { draft.metric = value })
  const [selected, setSelected] = createSignal("")
  const live = createMemo((previous: string) => props.revision ?? previous, "initial")
  const report = createLatest(() => ({ period: period(), by: by(), live: live() }), async (input, signal) => ({
    ...await ctx.main(Dashboard).usage({ period: input.period, by: input.by }, { signal }), group: input.by,
  }))
  const selectedKey = () => {
    const rows = report.latest?.rows ?? []
    if (rows.some((r) => r.key === selected())) return selected()
    return rows.length > 8 ? rows[0].key : ""
  }
  return <div class="exr-view">
    <Section title="Usage">
      <div class="exr-toolbar"><Segments label="Usage period" value={period()} options={periods} onChange={setPeriod} /><MenuSelect label="Group" value={by()} options={groups} onChange={(value) => { setBy(value); setSelected("") }} /></div>
      <ReadState loading={report.loading} error={report.error} hasData={!!report.latest} noun="usage" />
      <Show when={report.latest}>{(data) => <>
        <div class="exr-chart-toolbar"><Segments label="Activity metric" value={metric()} options={metrics} onChange={setMetric} />
          <Show when={data().rows.length > 1}><MenuSelect label="Series" value={selectedKey()} options={[...(data().rows.length <= 8 ? [{ value: "", label: "All groups" }] : []), ...data().rows.map((r) => ({ value: r.key, label: r.name }))]} onChange={setSelected} /></Show>
        </div>
        <ActivityChart report={data()} selected={selectedKey()} metric={metric()} now={props.now} />
        <p class="exr-caption">{date(data().from)} – {date(data().to)} · {data().timezone}</p>
      </>}</Show>
    </Section>
    <Show when={report.latest}>{(data) => <Section title="Breakdown" description={data().group === "token" ? "Only your API tokens are included." : undefined}>
      <List><For each={data().rows} fallback={<EmptyState title="No requests in this period" description="Try a wider time period to see earlier activity." />}>
        {(row) => <article class="exr-usage-row">
          <div class="exr-inline"><strong class="exr-wrap">{row.name}</strong><Badge>{count(row.requests)} requests</Badge></div>
          <div class="exr-counter-grid"><div><span>Input</span><strong>{count(row.input)}</strong></div><div><span>Output</span><strong>{count(row.output)}</strong></div><div><span>Cached input</span><strong>{count(row.cached)}</strong></div><div><span>Total</span><strong>{count(row.total)}</strong></div></div>
        </article>}
      </For></List>
    </Section>}</Show>
  </div>
}
