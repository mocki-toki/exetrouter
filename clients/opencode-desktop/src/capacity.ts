import type { Account, Snapshot } from "./schemas"
import { fresh } from "./dashboard"

export const horizon = 86400
const limit = 64, epsilon = 1e-8
type Meter = { kind: string; left: number; cost: number; reset: number; duration: number }
type Supply = { id: number; short: boolean; meters: Meter[]; expiry: number | null; credit: number | null; blockedUntil: number }
export type Gap = { from: number; to: number | null }
export type Outlook = {
  status: "unknown" | "risk" | "covered" | "idle" | "unavailable"
  reason: string; gap: Gap | null; higher: Gap | null; withCredit: Gap | null | undefined
  reduction: number | null; accounts: readonly Account[]; ready: number
  confidence: "Low" | "Medium"; observedAt: number; trend: number | null
  credits: { account: Account; expires: number; eligible: boolean }[]
}

export function capacityModels(report: Snapshot): string[] {
  return [...new Set(report.accounts.flatMap((a) => a.capacity?.models ?? []))].filter((id)=>id!=="gpt-image-2").sort()
}

/** When an already-blocked account can regain capacity. A short reset cannot
 * restore an exhausted weekly window, or bypass a non-quota operational pause. */
export function recovery(account: Account, now: number): number | null {
  if (account.enabled === false || account.capacity?.state !== "available" || account.capacity.shortWindow===null || account.refreshError || !account.windows.length) return null
  if (account.windows.some((w) => !fresh(w.observedAt, now) || w.status !== "current" || w.remaining === null || w.resetAt === null || w.resetAt <= now)) return null
  const blocked = account.windows.filter((w) => w.remaining! <= 0)
  return Math.max(now, account.cooldownUntil ?? now, ...blocked.map((w) => w.resetAt!))
}

function supply(account: Account, now: number): Supply | undefined {
  const p = account.capacity
  if (!p || p.shortWindow === null || account.refreshError || account.windows.length < 1 || account.windows.length > 2) return
  const windows = account.windows
  if (!windows.some((w) => w.minutes === 10080) || windows.some((w) => w.minutes === null || w.minutes <= 0 || w.minutes > 10080) || windows.some((w) => w.minutes! < 10080) !== p.shortWindow) return
  const meters: Meter[] = []
  for (const w of windows) {
    const cost = p.costs.find((c) => c.kind === w.kind)
    if (w.status !== "current" || !fresh(w.observedAt, now) || w.remaining === null || !Number.isFinite(w.remaining) || w.remaining<0 || w.remaining>100 || w.resetAt === null || !Number.isFinite(w.resetAt) || w.resetAt <= now || w.resetAt-now>w.minutes!*60+60 || !cost || cost.to !== w.observedAt || cost.perRequest === null || !Number.isFinite(cost.perRequest) || cost.perRequest <= 0) return
    meters.push({kind:w.kind,left:w.remaining,cost:cost.perRequest,reset:w.resetAt,duration:w.minutes!*60})
  }
  return {id:account.id,short:p.shortWindow,meters,expiry:fresh(account.credits.observedAt,now) ? Math.min(...account.credits.expiries.filter((at)=>at>now&&at<=now+3*86400),Infinity) : null,credit:p.resetEligibleUntil,blockedUntil:account.cooldownUntil??now}
}

function available(a: Supply, at: number): boolean { return a.blockedUntil<=at && a.meters.every((w) => w.left > epsilon) }
function deadline(a: Supply, at: number): number {
  const week = a.meters.find((w) => w.duration === 604800)!
  return Math.min(a.expiry!==null&&a.expiry>at?a.expiry:Infinity, week.left > epsilon && week.reset-at <= 86400 ? week.reset : Infinity)
}

/** Bounded minute-step estimate, not an admission/routing controller. Percentage
 * points are converted with each account's own observed request cost. Work is
 * redistributed on depletion using the router's urgent/short/reserve ordering.
 * Existing affinity, changed request sizes and external account use can diverge. */
export function simulate(input: readonly Supply[], requestsPerHour: number, now: number, until = now+horizon, credits = false): Gap | null {
  const accounts = input.map((a) => ({...a,meters:a.meters.map((w)=>({...w})),credited:false}))
  const perMinute = requestsPerHour/60
  for (let t=now; t<until; t+=60) {
    for (const a of accounts) {
      if (credits && !a.credited && a.credit !== null && a.credit>now+60 && t>=now+60) {
        // One already eligible credit after the maximum normal randomized delay.
        // This is a hypothetical successful reset, never confirmed supply.
        a.credited=true
        a.blockedUntil=t
        for (const w of a.meters) {w.left=100;w.reset=Infinity}
      }
      for (const w of a.meters) if (w.reset<=t) {w.left=100;w.reset=Infinity}
    }
    const budget = perMinute * Math.min(60,until-t)/60
    let pending = budget
    for (let pass=0; pending>epsilon && pass<=accounts.length; pass++) {
      const candidates=accounts.filter((a)=>available(a,t))
      if (!candidates.length) {
        const from=t+Math.min(60,until-t)*(1-pending/(budget||1))
        const next=accounts.map((a)=>Math.max(a.blockedUntil,...a.meters.filter((w)=>w.left<=epsilon).map((w)=>w.reset)))
        if (credits && t<now+60 && accounts.some((a)=>a.credit!==null&&a.credit>now+60)) next.push(now+60)
        const to=Math.min(...next.filter((at)=>at>from))
        return {from,to:Number.isFinite(to)?to:null}
      }
      const urgent=Math.min(...candidates.map((a)=>deadline(a,t)))
      const tier=Number.isFinite(urgent) ? candidates.filter((a)=>deadline(a,t)===urgent) : candidates.some((a)=>a.short) ? candidates.filter((a)=>a.short) : candidates
      const weight=(a:Supply) => {const week=a.meters.find((w)=>w.duration===604800)!;return week.left/Math.max(60,Number.isFinite(week.reset)?week.reset-t:week.duration)}
      const total=tier.reduce((n,a)=>n+weight(a),0)
      // Freeze weights for this allocation; consuming the first account must not
      // change the denominator/weights of later accounts in the same pass.
      const shares=tier.map((a)=>({a,work:pending*weight(a)/total}))
      let consumed=0
      for (const {a,work} of shares) {
        const take=Math.min(work,...a.meters.map((w)=>w.left/w.cost))
        for (const w of a.meters) {w.left=Math.max(0,w.left-take*w.cost);if(!Number.isFinite(w.reset)&&take>0)w.reset=t+w.duration}
        consumed+=take
      }
      pending=Math.max(0,pending-consumed)
    }
  }
  return null
}

export function capacityOutlook(report: Snapshot, model: string, now: number, disconnected = false): Outlook {
  const accounts=report.accounts.filter((a)=>a.capacity?.models.includes(model))
  const demand=report.capacity
  const result:Outlook={status:"unknown",reason:"Capacity outlook requires an updated router and native exr client.",gap:null,higher:null,withCredit:undefined,reduction:null,accounts,ready:0,confidence:"Low",observedAt:report.checkedAt,trend:demand&&demand.previousHour>0?(demand.lastHour/demand.previousHour-1)*100:null,credits:[]}
  if (disconnected || !fresh(report.checkedAt,now)) {result.reason="The snapshot is historical. Refresh before relying on this forecast.";return result}
  if (!demand) return result
  if (model==="gpt-image-2") {result.reason="Native image quotas are separate from text subscriptions and are not forecast here.";return result}
  if (!demand.complete || report.accounts.length>limit) {result.reason="The pool exceeds the bounded forecast size (64 accounts).";return result}
  if (!model || !accounts.length) {result.reason="No account catalog is available for this model.";return result}
  const eligible=accounts.filter((a)=>a.enabled!==false&&a.capacity?.state==="available")
  result.credits=eligible.flatMap((a)=>fresh(a.credits.observedAt,now)?a.credits.expiries.filter((at)=>at>now&&at<=now+3*86400).slice(0,a.credits.count??0).map((expires,index)=>({account:a,expires,eligible:index===0&&a.capacity?.resetEligibleUntil===expires})):[]).sort((a,b)=>a.expires-b.expires)
  if (!eligible.length) {result.status="unavailable";result.reason="No operationally available account supports this model. Quota resets cannot repair authentication or operational failures.";return result}
  if (eligible.some((a)=>a.refreshError||!a.windows.length||a.windows.some((w)=>w.status!=="current"||!fresh(w.observedAt,now)||w.remaining===null||w.resetAt===null||w.resetAt<=now))) {result.reason="Fresh, complete subscription windows are required for every eligible account.";return result}
  if (eligible.some((a)=>a.capacity?.shortWindow==null || !a.windows.some((w)=>w.minutes===10080) || a.windows.some((w)=>w.minutes===null||w.minutes<=0||w.minutes>10080) || a.windows.some((w)=>w.minutes!<10080)!==a.capacity.shortWindow)) {result.reason="The router has not confirmed the complete short/weekly window structure.";return result}
  result.observedAt=Math.min(report.checkedAt,...eligible.flatMap((a)=>a.windows.map((w)=>w.observedAt)))
  result.ready=eligible.filter((a)=>a.windows.every((w)=>w.remaining!>0)&&!(a.cooldownUntil!==null&&a.cooldownUntil>now)).length
  if (!result.ready) {
    const times=eligible.map((a)=>recovery(a,now)).filter((at):at is number=>at!==null&&at>now)
    result.status="risk";result.gap={from:now,to:times.length?Math.min(...times):null};result.higher=result.gap;result.reason="No confirmed subscription quota is available now."
  }
  const supplies=eligible.map((a)=>supply(a,now))
  if (supplies.some((a)=>!a)) {if(result.ready)result.reason="Not enough same-cycle quota changes and recent requests to calibrate every account. Unknown reserves are not counted as zero or unlimited.";return result}
  if (demand.lastHour<5) {if(result.ready){result.status="idle";result.reason="Too little recent traffic to estimate time remaining. No unlimited-capacity assumption is made."}return result}
  // Treat the whole recent text load as demand on this model's eligible pool.
  // This is conservative when other models can use a different set of accounts.
  const inputs=supplies as Supply[]
  result.gap=simulate(inputs,demand.lastHour,now)
  result.higher=simulate(inputs,Math.max(demand.lastHour,demand.previousHour)*1.25,now)
  if (inputs.some((a)=>a.credit!==null&&a.credit>now+60)) result.withCredit=simulate(inputs,demand.lastHour,now,now+horizon,true)
  result.confidence= demand.inFlight===0 && (result.trend===null||Math.abs(result.trend)<25) ? "Medium":"Low"
  result.status=result.gap||result.higher?"risk":"covered"
  result.reason="At current text load and recent average request mix; existing affinity and request sizes can change the result."
  if (result.gap && result.gap.from>now && result.gap.to!==null && result.gap.to<=now+horizon) {
    const until=Math.min(now+horizon,result.gap.to+60)
    if (!simulate(inputs,demand.lastHour*0.01,now,until)) {
      let low=0.01,high=1
      for(let i=0;i<8;i++){const mid=(low+high)/2;if(simulate(inputs,demand.lastHour*mid,now,until))high=mid;else low=mid}
      result.reduction=Math.min(99,Math.ceil((1-low)*100))
    }
  }
  return result
}
