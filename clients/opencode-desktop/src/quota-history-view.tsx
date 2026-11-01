import { For, Show, createMemo, createSignal } from "solid-js"
import type { Account } from "./schemas"
import { activityIndex } from "./chart"
import { quotaTimeline, quotaX, quotaY } from "./quota-history"
import { date, percent } from "./format"

export function QuotaHistoryView(props: { account: Account }) {
  const data = createMemo(() => quotaTimeline(props.account))
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
  const day = (at: number) => new Date(at * 1000).toLocaleDateString("en", { month: "short", day: "numeric" })
  const ticks = createMemo(() => [...new Set([0, 1, 2, 3, 4, 5, 6, 7].map((i) => Math.round(i / 7 * (data().hours.length - 1))))])
  return <section class="exr-section exr-history">
    <header class="exr-history-heading"><div><h3>Limit history</h3><p class="exr-caption">Last 7 days · hourly observations</p></div><div class="exr-history-legend"><For each={data().series}>{(line) => <span><i style={{ background: line.color }} />{line.label}</span>}</For></div></header>
    <Show when={data().series.length > 0} fallback={<p class="exr-caption">{props.account.history ? "No recorded observations yet." : "Hourly history requires an updated router and native exr client."}</p>}>
      <div class="exr-chart"><div class="exr-inline exr-caption"><span>Used quota</span><span>0–100%</span></div>
        <div class="exr-chart-plot"><svg viewBox="0 0 668 240" role="img" tabindex="0" aria-label="Hourly subscription quota history. Left and right select an hour; Home and End select the first and latest hour." onPointerMove={move} onPointerLeave={() => setSelected(undefined)} onKeyDown={keyboard} onBlur={() => setSelected(undefined)}>
          <For each={[0, 25, 50, 75, 100]}>{(value) => <g><line class="exr-chart-grid" x1="44" x2="644" y1={quotaY(value)} y2={quotaY(value)} /><text x="34" y={quotaY(value) + 4} text-anchor="end">{value}%</text></g>}</For>
          <For each={ticks()}>{(i) => <text x={quotaX(i, data().hours.length)} y="231" text-anchor={i === 0 ? "start" : i === data().hours.length - 1 ? "end" : "middle"}>{day(data().hours[i])}</text>}</For>
          <For each={data().series}>{(line) => <g data-window={line.kind}><For each={line.segments}>{(points) => <polyline points={points} fill="none" stroke={line.color} stroke-width="1.7" stroke-linejoin="round" />}</For><For each={line.isolated}>{(p) => <circle cx={p.x} cy={p.y} r="2.5" fill={line.color} />}</For></g>}</For>
          <Show when={cursor() !== undefined}><g class="exr-chart-cursor"><line x1={cursor()} x2={cursor()} y1="20" y2="204" /><For each={data().series}>{(line) => <Show when={line.points.get(selected()!)}>{(p) => <circle cx={cursor()} cy={quotaY(p().used)} r="3.5" fill={line.color} />}</Show>}</For></g></Show>
        </svg>
        <Show when={cursor() !== undefined}><div class="exr-chart-tooltip exr-history-tooltip" role="status" style={{ left: cursor()! <= 334 ? `calc(${cursor()! / 668 * 100}% + 24px)` : "auto", right: cursor()! > 334 ? `calc(${100 - cursor()! / 668 * 100}% + 24px)` : "auto", "max-width": `calc(${Math.max(cursor()!, 668-cursor()!) / 668 * 100}% - 32px)` }}><strong>{date(selected())}</strong><For each={data().series}>{(line) => <div><span class="exr-inline-start"><i style={{ background: line.color }} />{line.label}</span><Show when={line.points.get(selected()!)} fallback={<span class="exr-caption">Not observed</span>}>{(p) => <span>{percent(p().used)} used <span class="exr-caption">· {percent(Math.max(0, 100 - p().used))} left</span></span>}</Show></div>}</For></div></Show>
        </div>
        <p class="exr-caption exr-history-caption">Latest observation · {date(data().latest)}</p>
      </div>
    </Show>
  </section>
}
