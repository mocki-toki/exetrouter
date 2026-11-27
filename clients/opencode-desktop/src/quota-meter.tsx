import { Show, type JSX } from "solid-js"
import { percent } from "./format"

export function QuotaMeter(props: {
  label: string
  remaining: number | null
  historical?: boolean
  title?: string
  meterLabel: string
  children: JSX.Element
}) {
  const tone = () => props.remaining === null || props.historical ? "neutral" : props.remaining <= 20 ? "danger" : props.remaining <= 50 ? "warning" : "success"
  return <div class="exr-quota" data-tone={tone()}>
    <div class="exr-inline"><span title={props.title}>{props.label}</span><strong>{percent(props.remaining)}<Show when={props.remaining !== null}><span class="exr-caption"> left</span></Show></strong></div>
    <div class="exr-quota-track" role={props.remaining === null ? "img" : "meter"} aria-label={`${props.meterLabel}${props.remaining === null ? ": unknown" : ""}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={props.remaining ?? undefined}>
      <Show when={props.remaining !== null}><span style={{ width: `${props.remaining}%` }} /></Show>
    </div>
    <div class="exr-inline exr-caption">{props.children}</div>
  </div>
}
