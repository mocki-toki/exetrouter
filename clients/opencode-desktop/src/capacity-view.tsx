import { For, Show, createMemo, createSignal } from "solid-js"
import type { Account, Snapshot, Window } from "./schemas"
import { capacityModels, capacityOutlook, horizon, recovery, type Gap, type Outlook } from "./capacity"
import { fresh } from "./dashboard"
import { count, date, duration, percent } from "./format"
import { Badge, Fields, MenuSelect, Section, useDialog, useHost } from "./ui"

export function timeLeft(seconds: number): string {
  const minutes=Math.max(0,Math.ceil(seconds/60))
  return minutes>=1440?`${Math.floor(minutes/1440)}d ${Math.floor(minutes%1440/60)}h`:minutes>=60?`${Math.floor(minutes/60)}h ${minutes%60}m`:`${minutes}m`
}
function interruption(gap: Gap | null, now: number): string {
  if (!gap) return "No gap estimated in the next 24 hours"
  return `${gap.from<=now?"Now":date(gap.from)} – ${gap.to===null?"Recovery unknown":date(gap.to)}${gap.to===null?"":` · ~${timeLeft(gap.to-gap.from)}`}`
}
const states:Record<string,string>={available:"Available",disabled:"Deactivated",reauth_required:"Reauthorization required",unconfigured:"Router not configured",backoff:"Operational backoff",refresh_due:"Credentials need refresh",catalog_unavailable:"Catalog unavailable",cooldown:"Operational cooldown"}

function CapacityDetails(props: {report:Snapshot;outlook:Outlook;model:string;onModel(value:string):void;now:number}) {
  const data=()=>props.outlook
  const window=(a:Account,weekly:boolean)=>a.windows.find((w)=>weekly?w.minutes===10080:w.minutes!==null&&w.minutes<10080)
  const meter=(w:Window|undefined)=><Show when={w} fallback={<span class="exr-caption">Not reported</span>}>{(value)=><><strong>{percent(value().remaining)}</strong><span class="exr-caption">{duration(value().minutes)} · resets {date(value().resetAt)}{value().status!=="current"||!fresh(value().observedAt,props.now)?" · historical":""}</span></>}</Show>
  const status=(a:Account)=>a.capacity?.state!=="available"?states[a.capacity?.state??""]??"Unknown":recovery(a,props.now)===null?"Unknown":recovery(a,props.now)!<=props.now?"Available now":date(recovery(a,props.now))
  return <div class="exr exr-dialog-content">
    <MenuSelect label="Model" value={props.model} options={capacityModels(props.report).map((value)=>({value,label:value}))} onChange={props.onModel}/>
    <Section title="Account reserves" description="Only accounts whose catalog supports the selected model. Percentages across subscriptions are never added.">
      <div class="exr-capacity-table"><table><thead><tr><th>Account</th><th>Short window left</th><th>Weekly left</th><th>Availability / recovery</th></tr></thead><tbody><For each={data().accounts}>{(a)=><tr><th scope="row">{a.label}</th><td>{meter(window(a,false))}</td><td>{meter(window(a,true))}</td><td>{status(a)}</td></tr>}</For></tbody></table></div>
      <Show when={!data().accounts.length}><p class="exr-caption">No model-scoped account metadata is available.</p></Show>
    </Section>
    <Section title="Consumption">
      <Fields rows={[["Text requests · last hour",props.report.capacity?count(props.report.capacity.lastHour):"Unknown"],["Versus previous hour",data().trend===null?"Unknown":`${data().trend!>=0?"+":""}${Math.round(data().trend!)}%`],["Estimated runway",data().gap?`~${timeLeft(data().gap!.from-props.report.checkedAt)}`:data().status==="covered"||data().status==="risk"?"At least 24 hours":"Unknown"],["Confidence",data().confidence],["Observed",date(data().observedAt)]]}/>
      <p class="exr-caption">{data().reason}</p>
    </Section>
    <Section title="Reset credits" description="Not included in confirmed-quota capacity. No credits are consumed by opening this view.">
      <For each={data().credits} fallback={<p class="exr-caption">No fresh expiring credit details reported for available accounts.</p>}>{(credit)=><div class="exr-detail-group"><strong>{credit.account.label}</strong><Fields rows={[["Expires",date(credit.expires)],["Automatic reset",credit.eligible?"Eligible at snapshot · recheck required":credit.account.capacity?.resetEligibleUntil!==null?"Not included in this scenario":"Waiting for eligibility or recovery confirmation"]]}/></div>}</For>
    </Section>
    <Section title="Scenarios · next 24 hours" description="First estimated interruption in each scenario; later gaps are not listed.">
      <Fields rows={[["Confirmed quota only",["covered","risk"].includes(data().status)?interruption(data().gap,props.report.checkedAt):"Insufficient data"],["Higher load (+25%)",["covered","risk"].includes(data().status)&&props.report.capacity?.lastHour?interruption(data().higher,props.report.checkedAt):"Insufficient data"],["With eligible reset credit",data().withCredit===undefined?"Not calibrated / no confirmed eligible credit":interruption(data().withCredit!,props.report.checkedAt)]]}/>
      <Show when={data().reduction!==null}><p>Estimated load reduction: ~{data().reduction}% until {date(data().gap?.to)}.</p></Show>
      <p class="exr-caption">The reset scenario assumes one successful, currently eligible reset per account after the normal recheck delay, with observed recovery. Future eligibility, strategic credits and repeated credit spending are not assumed.</p>
    </Section>
    <Section title="How this estimate works"><p class="exr-caption">Each account's quota changes are calibrated against its own recent text-request count, within the same quota cycle. The entire recent text load is projected onto this model's eligible pool, using recent average request sizes and mix. Natural resets and both window constraints are simulated; short-window resets cannot replenish an exhausted week. Hourly gaps, missing calibration, unknown window structure and stale data suppress the forecast. Shared account use outside this router, model-specific limits, changed Fast mode, request mix and pinned contexts can change actual availability. This is a planning estimate, not a guarantee or an admission limit.</p></Section>
  </div>
}

export function CapacityCard(props:{report:Snapshot;now:number;disconnected:boolean}) {
  const ctx=useHost(), ui=ctx.ui, open=useDialog()
  const [selected,setSelected]=createSignal("")
  const models=createMemo(()=>capacityModels(props.report))
  const model=createMemo(()=>models().includes(selected())?selected():models().includes("gpt-6.1-sol")?"gpt-6.1-sol":models()[0]??"")
  // Simulation only changes with a snapshot/model, not every clock tick.
  const estimate=createMemo(()=>capacityOutlook(props.report,model(),props.report.checkedAt))
  const historical=()=>props.disconnected||!fresh(props.report.checkedAt,props.now)||estimate().accounts.some((a)=>a.enabled!==false&&a.capacity?.state==="available"&&a.windows.some((w)=>!fresh(w.observedAt,props.now)||w.resetAt!==null&&w.resetAt<=props.now))
  const data=createMemo(()=>{
    const value=estimate()
    if(historical())return {...value,status:"unknown" as const,confidence:"Low" as const,gap:null,higher:null,withCredit:undefined,reduction:null,credits:[],reason:"The snapshot is historical. Refresh before relying on this forecast."}
    const credits=value.credits.filter((c)=>c.expires>props.now&&fresh(c.account.credits.observedAt,props.now))
    return {...value,credits,withCredit:credits.some((c)=>c.eligible&&c.expires>props.now+60)?value.withCredit:undefined}
  })
  const gap=()=>data().gap??data().higher
  const details=()=>open((dialog)=><ui.Dialog title="Capacity details" description="Model-scoped planning estimate · confirmed quota and conditional reset scenarios" size="large" footer={<ui.Button onClick={dialog.close}>Done</ui.Button>}><CapacityDetails report={props.report} outlook={data()} model={model()} onModel={setSelected} now={props.now}/></ui.Dialog>)
  return <section class="exr-capacity" data-tone={data().status==="risk"||data().status==="unavailable"?"warning":"neutral"}>
    <header class="exr-inline"><h3>Capacity outlook</h3><Badge tone={data().status==="risk"||data().status==="unavailable"?"warning":"neutral"}>{({risk:"At risk",covered:"No gap forecast",unknown:"Insufficient data",idle:"Low activity",unavailable:"Unavailable"})[data().status]}</Badge></header>
    <p class="exr-caption">{model()||"Text subscriptions"} · At current text load</p>
    <Show when={data().status==="covered"||data().status==="risk"} fallback={<p class="exr-caption">{historical()?"Refresh to update the forecast.":!props.report.capacity?"Update the router and native exr to enable forecasts.":data().status==="unavailable"?"No available account for this model.":data().status==="idle"?"Not enough recent traffic to estimate runway.":"Not enough fresh quota history to estimate runway."}</p>}>
      <Fields rows={[["Estimated runway",data().gap?`~${timeLeft(data().gap!.from-props.now)}`:`At least ${timeLeft(horizon)}`],...(gap()?[[data().gap?"Possible interruption":"At higher load (+25%)",interruption(gap(),props.now)] as const]:[])]}/>
    </Show>
    <Show when={data().credits.length>0}><p class="exr-caption">{data().credits.length} expiring reset {data().credits.length===1?"credit":"credits"} · next expires {date(data().credits[0].expires)}</p></Show>
    <footer class="exr-inline"><span class="exr-caption"><Show when={data().reduction!==null}>Consider ~{data().reduction}% less load until {date(data().gap?.to)}.</Show></span><ui.Button size="small" onClick={details}>Details</ui.Button></footer>
  </section>
}
