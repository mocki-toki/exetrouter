import { createSignal, type JSX } from "solid-js"
import { Segments, useHost } from "./ui"

export const periods = [{ value: "24h", label: "24 hours" }, { value: "7d", label: "7 days" }, { value: "30d", label: "30 days" }, { value: "90d", label: "90 days" }] as const
export type Period = typeof periods[number]["value"]
export function rollingPeriod(value: string): Period {
  return value === "week" ? "7d" : value === "month" ? "30d" : periods.some((p) => p.value === value) ? value as Period : "24h"
}
export function span(period: Period): number { return (period === "24h" ? 1 : Number(period.slice(0, -1))) * 86400 }

export function RangeControls(props: { label: string; period: Period; before?: number; end: number; now: number; loading?: boolean; children?: JSX.Element; onPeriod(value: Period): void; onBefore(value: number | undefined): void }) {
  const ui = useHost().ui
  const [anchor, setAnchor] = createSignal<number>()
  return <div class="exr-toolbar">
    <Segments label={`${props.label} period`} value={props.period} options={periods} onChange={props.onPeriod} />
    <div class="exr-inline-start exr-range-actions">
      <ui.IconButton icon="chevron-left" label={`Previous ${props.label.toLowerCase()} interval`} disabled={props.loading || props.end < span(props.period) * 2} onClick={() => { if (props.before === undefined) setAnchor(props.end); props.onBefore(props.end - span(props.period)) }} />
      <ui.IconButton icon="chevron-right" label={`Next ${props.label.toLowerCase()} interval`} disabled={props.loading || props.before === undefined} onClick={() => { const next = props.end + span(props.period); props.onBefore(next >= (anchor() ?? props.now) ? undefined : next) }} />
      {props.children}
    </div>
  </div>
}
