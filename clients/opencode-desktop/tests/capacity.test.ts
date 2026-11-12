import assert from "node:assert/strict"
import { test } from "node:test"
import { capacityModels, capacityOutlook, recovery } from "../src/capacity"
import { snapshot } from "../src/native"
import type { Account, Snapshot } from "../src/schemas"
import { doctor } from "./fixtures"

const now=1800000000, hour=3600
function account(id=1, left=1, short=true):Account {
  const windows=[...(short?[{kind:"primary",minutes:300,remaining:100,used:0,resetAt:now+hour/2,observedAt:now,status:"current"}]:[]),{kind:"secondary",minutes:10080,remaining:left,used:100-left,resetAt:now+3*hour,observedAt:now,status:"current"}]
  return {...snapshot(doctor()).accounts[0],id,label:`Synthetic ${id}`,cooldownUntil:null,refreshError:false,windows,
    capacity:{state:"available",shortWindow:short,models:["sol","astra"],resetEligibleUntil:null,costs:windows.map((w)=>({kind:w.kind,perRequest:0.1,from:now-hour,to:now,requests:10}))},credits:{count:0,observedAt:now,expiries:[]},automaticReset:"idle"}
}
function report(accounts:readonly Account[]=[account()]):Snapshot {
  return {...snapshot(doctor()),checkedAt:now,accounts,capacity:{lastHour:10,previousHour:10,inFlight:0,complete:true}}
}
test("capacity forecasts pool exhaustion, useful recovery and an actionable load reduction",()=>{
  const r=report(), p=capacityOutlook(r,"sol",now)
  assert.equal(p.status,"risk")
  assert.ok(Math.abs(p.gap!.from-(now+hour))<60)
  assert.equal(p.gap!.to,now+3*hour,"a short reset cannot restore the depleted week")
  assert.ok(p.reduction!==null&&p.reduction>=66&&p.reduction<=68)
  assert.ok(p.higher!.from<p.gap!.from)
  assert.equal(p.withCredit,undefined)
  assert.equal(p.confidence,"Medium")
})
test("separate calibrated plans and weekly-only reserves are used through exhaustion",()=>{
  const later=(a:Account)=>({...a,windows:a.windows.map((w)=>w.minutes===10080?{...w,resetAt:now+6*hour}:w)})
  const first=later(account()), reserve={...later(account(2,2,false)),capacity:{...account(2,2,false).capacity!,models:["sol"]}}
  const r=report([first,reserve]), sol=capacityOutlook(r,"sol",now), astra=capacityOutlook(r,"astra",now)
  assert.ok(Math.abs(sol.gap!.from-(now+3*hour))<60)
  assert.ok(astra.gap!.from<sol.gap!.from,"unsupported model capacity is not pooled")
  assert.equal(sol.accounts.length,2)
  assert.equal(astra.accounts.length,1)
  assert.equal(JSON.stringify(r),JSON.stringify(report([first,reserve])),"simulation never changes the source snapshot")
})
test("stale, unknown, idle and missing calibration never become safe or unlimited capacity",()=>{
  const r=report()
  assert.equal(capacityOutlook(r,"sol",now+60).status,"unknown")
  assert.equal(capacityOutlook(r,"sol",now,true).status,"unknown")
  assert.equal(capacityOutlook({...r,capacity:undefined},"sol",now).status,"unknown")
  const a=account()
  assert.equal(capacityOutlook(report([{...a,capacity:{...a.capacity!,shortWindow:null}}]),"sol",now).status,"unknown")
  assert.equal(capacityOutlook(report([{...a,capacity:{...a.capacity!,costs:[]}}]),"sol",now).status,"unknown")
  assert.equal(capacityOutlook(report([{...a,windows:a.windows.map((w)=>({...w,status:"stale"}))}]),"sol",now).status,"unknown")
  assert.equal(capacityOutlook({...r,capacity:{...r.capacity!,lastHour:0}},"sol",now).status,"idle")
  assert.equal(capacityOutlook({...r,capacity:{...r.capacity!,complete:false}},"sol",now).status,"unknown")
  const images=report([{...a,capacity:{...a.capacity!,models:["gpt-image-2","sol"]}}])
  assert.deepEqual(capacityModels(images),["sol"])
  assert.equal(capacityOutlook(images,"gpt-image-2",now).status,"unknown")
})
test("currently exhausted accounts recover only after every blocking window",()=>{
  const a=account(), blocked={...a,windows:a.windows.map((w)=>({...w,remaining:0,used:100}))}
  assert.equal(recovery(blocked,now),now+3*hour)
  const p=capacityOutlook(report([blocked]),"sol",now)
  assert.deepEqual(p.gap,{from:now,to:now+3*hour})
  assert.deepEqual(p.higher,p.gap)
  assert.equal(p.reduction,null,"lowering traffic cannot repair already exhausted quota")
  const backoff={...blocked,capacity:{...blocked.capacity!,state:"backoff"}}
  assert.equal(recovery(backoff,now),null)
  assert.equal(capacityOutlook(report([backoff]),"sol",now).status,"unavailable")
})
test("only a current ledger-checked credit changes the separate conditional scenario",()=>{
  const a=account(), eligible={...a,capacity:{...a.capacity!,resetEligibleUntil:now+2*hour},credits:{count:1,observedAt:now,expiries:[now+2*hour]}}
  const baseline=capacityOutlook(report([a]),"sol",now), withCredit=capacityOutlook(report([eligible]),"sol",now)
  assert.deepEqual(withCredit.gap,baseline.gap,"a credit never increases confirmed supply")
  assert.equal(withCredit.withCredit,null,"a successful reset removes the estimated gap in this fixture")
  assert.equal(withCredit.credits[0].eligible,true)
  const waiting={...eligible,capacity:{...eligible.capacity!,resetEligibleUntil:null},automaticReset:"unknown"}
  assert.equal(capacityOutlook(report([waiting]),"sol",now).withCredit,undefined)
  const strategic={...eligible,capacity:{...eligible.capacity!,resetEligibleUntil:null},credits:{...eligible.credits,expiries:[now+30*86400]}}
  assert.equal(capacityOutlook(report([strategic]),"sol",now).credits.length,0)
  const empty={...eligible,windows:eligible.windows.map((w)=>({...w,remaining:0,used:100}))}
  const recoveryScenario=capacityOutlook(report([empty]),"sol",now)
  assert.equal(recoveryScenario.gap!.from,now)
  assert.equal(recoveryScenario.withCredit!.to,now+60,"an eligible reset is still a conditional future event, never immediate confirmed capacity")
})
test("natural short-window recovery and varying account costs do not sum raw percentages",()=>{
  const a=account(1,100), b=account(2,100,false)
  const supplied={...a,windows:a.windows.map((w)=>w.kind==="primary"?{...w,remaining:1,used:99}:w)}
  const p=capacityOutlook(report([supplied,b]),"sol",now)
  assert.equal(p.gap,null,"a usable reserve bridges the short-window reset")
  assert.equal(p.status,"covered")
  const expensive={...account(1,1,false),capacity:{...account(1,1,false).capacity!,costs:[{kind:"secondary",perRequest:1,from:now-hour,to:now,requests:10}]}}
  const cheaper=account(2,1,false)
  const different=capacityOutlook(report([expensive,cheaper]),"sol",now)
  assert.ok(Math.abs(different.gap!.from-(now+1.1*hour))<60,"1 request on one plan + 10 on another, not 2% pooled")
})
