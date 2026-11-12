import { For, Show, createMemo, createSignal } from "solid-js"
import type { Account } from "./schemas"
import { activityIndex } from "./chart"
import { quotaLeft, quotaTimeline, quotaX, quotaY } from "./quota-history"
import { date, percent } from "./format"
import { createLatest } from "@opencode/plugin/gui"
import { Dashboard } from "./contract"
import { RangeControls, type Period } from "./time-range"
import { ReadState, useHost } from "./ui"

export function QuotaHistoryView(props: { account: Account }) {
  const ctx = useHost()
  const [period, setPeriod] = createSignal<Period>("7d"), [before, setBefore] = createSignal<number>()
  const latestEnd = () => props.account.history?.to ?? Math.floor(Date.now() / 3600000) * 3600 + 3600
  const report = createLatest(() => ({ account: props.account.id, period: period(), before: before(), live: props.account.history }), async (input, signal) => ({
    history: await ctx.main(Dashboard).quotaHistory({account: input.account,period:input.period,...(input.before === undefined ? {} : {before:input.before})},{signal}), key: `${input.account}:${input.period}:${input.before ?? "latest"}`,
  }))
  const history = createMemo(() => {
    const fetched = report.latest?.key === `${props.account.id}:${period()}:${before() ?? "latest"}` ? report.latest.history : null
    const snapshot = period() === "7d" && before() === undefined ? props.account.history : null
    const newest = (h: Account["history"]) => Math.max(0, ...(h?.points.map((p) => p.observedAt) ?? []))
    return snapshot && (!fetched || snapshot.to >= fetched.to && newest(snapshot) >= newest(fetched)) ? snapshot : fetched
  })
  const data = createMemo(() => quotaTimeline({...props.account,history:history()}))
  // Retain the selected absolute hour through background snapshots, not its shifting index.
  const [selected, setSelected] = createSignal<number>()
  const index = () => selected() === undefined ? -1 : data().hours.indexOf(selected()!)
  const cursor = () => index() >= 0 ? quotaX(index(), data().hours.length) : undefined
  const move = (event: PointerEvent & { currentTarget: SVGSVGElement }) => {
    const rect = event.currentTarget.getBoundingClientRect()
    const i = activityIndex(data().hours.length, event.clientX, rect.left, rect.width)
    if (i !== undefined) setSelected(data().hours[i])
  }
  const keyboard = (event: KeyboardEvent) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End", "Escape"].includes(event.key)) return
    event.preventDefault()
    if (event.key === "Escape") { setSelected(undefined); return }
    const last = data().hours.length - 1
    const next = event.key === "Home" ? 0 : event.key === "End" ? last : Math.max(0, Math.min(last, (index() < 0 ? last : index()) + (event.key === "ArrowLeft" ? -1 : 1)))
    setSelected(data().hours[next])
  }
  const day = (at: number) => new Date(at * 1000).toLocaleString("en", period() === "24h" ? {hour:"2-digit",minute:"2-digit"} : { month: "short", day: "numeric" })
  const ticks = createMemo(() => [...new Set([0, 1, 2, 3, 4, 5, 6, 7].map((i) => Math.round(i / 7 * (data().hours.length - 1))))])
  return <section class="exr-section exr-history">
    <header class="exr-history-heading"><div><h3>Limit history</h3><p class="exr-caption">Hourly observations · unavailable hours remain gaps</p></div><div class="exr-history-legend"><For each={data().series}>{(line) => <span><i style={{ background: line.color }} />{line.label}</span>}</For></div></header>
    <RangeControls label="Limit history" period={period()} before={before()} end={before() ?? history()?.to ?? latestEnd()} now={latestEnd()} loading={report.loading} onPeriod={(value) => { setBefore(undefined); setPeriod(value) }} onBefore={setBefore} />
    <ReadState loading={report.loading} error={report.error} hasData={!!history()} noun="limit history" />
    <Show when={history()}>{(h) => <p class="exr-caption">{date(h().from)} – {date(h().to)}</p>}</Show>
    <Show when={data().series.length > 0} fallback={<p class="exr-caption">{props.account.history ? "No recorded observations yet." : "Hourly history requires an updated router and native exr client."}</p>}>
      <div class="exr-chart"><div class="exr-inline exr-caption"><span>Quota left</span><span>0–100%</span></div>
        <div class="exr-chart-plot"><svg viewBox="0 0 668 240" role="img" tabindex="0" aria-label="Hourly subscription quota left. Left and right select an hour; Home and End select the first and latest hour." onPointerMove={move} onPointerLeave={() => setSelected(undefined)} onKeyDown={keyboard} onBlur={() => setSelected(undefined)}>
          <For each={[0, 25, 50, 75, 100]}>{(value) => <g><line class="exr-chart-grid" x1="44" x2="644" y1={quotaY(value)} y2={quotaY(value)} /><text x="34" y={quotaY(value) + 4} text-anchor="end">{value}%</text></g>}</For>
          <For each={ticks()}>{(i) => <text x={quotaX(i, data().hours.length)} y="231" text-anchor={i === 0 ? "start" : i === data().hours.length - 1 ? "end" : "middle"}>{day(data().hours[i])}</text>}</For>
          <For each={data().series}>{(line) => <g data-window={line.kind}><For each={line.segments}>{(points) => <polyline points={points} fill="none" stroke={line.color} stroke-width="1.7" stroke-linejoin="round" />}</For><For each={line.isolated}>{(p) => <circle cx={p.x} cy={p.y} r="2.5" fill={line.color} />}</For></g>}</For>
          <Show when={cursor() !== undefined}><g class="exr-chart-cursor"><line x1={cursor()} x2={cursor()} y1="20" y2="204" /><For each={data().series}>{(line) => <Show when={line.points.get(selected()!)}>{(p) => <circle cx={cursor()} cy={quotaY(quotaLeft(p().used))} r="3.5" fill={line.color} />}</Show>}</For></g></Show>
        </svg>
        <Show when={cursor() !== undefined}><div class="exr-chart-tooltip exr-history-tooltip" role="status" style={{ left: cursor()! <= 334 ? `calc(${cursor()! / 668 * 100}% + 24px)` : "auto", right: cursor()! > 334 ? `calc(${100 - cursor()! / 668 * 100}% + 24px)` : "auto", "max-width": `calc(${Math.max(cursor()!, 668-cursor()!) / 668 * 100}% - 32px)` }}><strong>{date(selected())}</strong><For each={data().series}>{(line) => <div><span class="exr-inline-start"><i style={{ background: line.color }} />{line.label}</span><Show when={line.points.get(selected()!)} fallback={<span class="exr-caption">Not observed</span>}>{(p) => <span>{percent(quotaLeft(p().used))} left</span>}</Show></div>}</For></div></Show>
        </div>
        <p class="exr-caption exr-history-caption">Latest observation · {date(data().latest)}</p>
      </div>
    </Show>
  </section>
}
