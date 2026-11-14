import { For, Show, createMemo, createSignal } from "solid-js"
import type { PlatformHistory } from "./schemas"
import { date, dollars } from "./format"

/** Costs have a real daily resolution, not fabricated ten-minute quota samples. */
export function PlatformHistoryChart(props: { history: PlatformHistory }) {
  const [selected,setSelected]=createSignal<number>()
  const points=()=>props.history.points
  const maximum=createMemo(()=>Math.max(.01,...points().map(p=>p.usd)))
  const x=(at:number)=>44+(at-props.history.from)/(props.history.to-props.history.from)*600
  const active=()=>selected()===undefined?undefined:points()[selected()!]
  return <div class="exr-chart exr-platform-history">
    <header class="exr-history-heading"><div><h3>Platform API costs</h3><p class="exr-caption">Organization-wide daily USD · not a remaining balance</p></div></header>
    <Show when={points().length} fallback={<p class="exr-caption">No recorded costs in this interval. Missing daily buckets are not treated as $0.</p>}>
      <div class="exr-chart-plot"><svg viewBox="0 0 668 220" role="img" tabindex="0" aria-label="Platform API daily costs. Left and right select a day; Home and End select first and latest."
        onKeyDown={(event)=>{
          const last=points().length-1
          if (event.key==="Escape") {setSelected(undefined);return}
          const next=event.key==="Home"?0:event.key==="End"?last:event.key==="ArrowLeft"?Math.max(0,(selected()??last)-1):event.key==="ArrowRight"?Math.min(last,(selected()??last)+1):undefined
          if (next!==undefined) {event.preventDefault();setSelected(next)}
        }} onBlur={()=>setSelected(undefined)} onPointerLeave={()=>setSelected(undefined)}>
        <For each={[0,.25,.5,.75,1]}>{ratio=><g><line x1="44" x2="644" y1={184-ratio*164} y2={184-ratio*164} class="exr-chart-grid"/><text x="36" y={188-ratio*164} text-anchor="end">{dollars(maximum()*ratio)}</text></g>}</For>
        <For each={points()}>{(p,i)=><rect x={x(p.from)+1} y={184-p.usd/maximum()*164} width={Math.max(1,x(p.to)-x(p.from)-2)} height={Math.max(1,p.usd/maximum()*164)} fill="var(--exr-history-api)" opacity={selected()===i()?1:.65} onPointerMove={()=>setSelected(i())}><title>{date(p.from)} · {dollars(p.usd)}</title></rect>}</For>
        <For each={[props.history.from,props.history.to]}>{at=><text x={x(at)} y="211" text-anchor={at===props.history.from?"start":"end"}>{date(at)}</text>}</For>
      </svg><Show when={active()}>{p=><div class="exr-caption" role="status">{date(p().from)} – {date(p().to)} · {dollars(p().usd)} · observed {date(p().observedAt)}</div>}</Show></div>
    </Show>
  </div>
}
