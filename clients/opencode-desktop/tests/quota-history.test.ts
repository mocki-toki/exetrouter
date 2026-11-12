import assert from "node:assert/strict"
import { test } from "node:test"
import { doctor, usage } from "./fixtures"
import { quotaHistory, snapshot, usage as normalizeUsage } from "../src/native"
import { quotaLeft, quotaTimeline, quotaY } from "../src/quota-history"
import { activity } from "../src/chart"

test("hourly quota history keeps both independent windows, actual samples and gaps", () => {
  const account = snapshot(doctor()).accounts[0]
  const data = quotaTimeline(account)
  assert.equal(data.hours.length,168)
  assert.deepEqual(data.series.map((s)=>s.label),["8-hour","Weekly"])
  assert.notEqual(data.series[0].color,data.series[1].color)
  assert.equal(data.series[0].points.size,2)
  assert.equal(data.series[0].points.get(data.hours[0]),undefined)
  assert.equal(data.series[0].segments.length,1)
  const h = account.history!
  const gapped = {...account,history:{...h,points:[...h.points,{...h.points[0],hour:h.from,observedAt:h.from+60}]}}
  const line = quotaTimeline(gapped).series[0]
  assert.equal(line.isolated.length,1)
  assert.equal(line.segments.length,1,"missing hours must break the line instead of bridging gaps")
  assert.equal(quotaTimeline({...account,history:null}).hours.length,0)
})
test("quota charts omit series without a known duration and retain known-window fallback", () => {
  const account = snapshot(doctor()).accounts[0]
  const history = account.history!
  const points = [
    ...history.points.map((point) => point.kind === "primary" ? { ...point, minutes: null } : point),
    { ...history.points[0], kind: "unknown", minutes: null },
  ]
  const data = quotaTimeline({ ...account, history: { ...history, points } })
  assert.deepEqual(data.series.map((line) => line.label), ["8-hour", "Weekly"])
  assert.equal(data.series.some((line) => line.kind === "unknown"), false)
  const unknownOnly = quotaTimeline({ ...account, windows: [], history: { ...history, points: [points.at(-1)!] } })
  assert.deepEqual(unknownOnly.series, [])
})
test("native history projection is bounded and strips unknown payloads", () => {
  const raw=doctor().quota_accounts[0].quota_history
  assert.equal(quotaHistory(null),null)
  assert.doesNotMatch(JSON.stringify(quotaHistory({...raw,payload:"private-marker",points:raw.points.map((p)=>({...p,payload:"private-marker"}))})),/private-marker|payload/)
  for (const bad of [
    {...raw,interval_seconds:86400}, {...raw,to_utc:raw.from_utc+2161*3600},
    {...raw,points:[...raw.points,raw.points[0]]},
    {...raw,points:[{...raw.points[0],used_percent:-1}]},
    {...raw,points:[{...raw.points[0],observed_at:raw.points[0].hour-1}]},
    {...raw,points:Array(4321).fill(raw.points[0])},
  ]) assert.throws(()=>quotaHistory(bad))
})
test("ten-minute quota history covers the full range, gaps and legacy hourly reports", () => {
  const account = snapshot(doctor()).accounts[0]
  const from = 1800000000, to = from + 90 * 86400
  const raw = { from_utc:from, to_utc:to, interval_seconds:600, points:Array.from({length:12960},(_,i)=>["primary","secondary"].map((kind)=>({kind,hour:from+i*600,observed_at:from+i*600+599,used_percent:20,window_minutes:kind==="primary"?480:10080,reset_at:null}))).flat() }
  const history = quotaHistory(raw)!
  assert.equal(history.points.length,25920)
  assert.equal(history.interval,600)
  const data = quotaTimeline({...account,history})
  assert.equal(data.hours.length,12960)
  assert.equal(data.hours[1]-data.hours[0],600)
  assert.equal(data.hours.at(-1),to-600)
  assert.equal(data.series[0].segments.length,1)
  const gapped = quotaTimeline({...account,history:{...history,points:history.points.filter((p)=>p.hour!==from+600)}})
  assert.equal(gapped.series[0].isolated.length,1)
  assert.equal(gapped.series[0].segments.length,1)
  assert.throws(()=>quotaHistory({...raw,points:[{...raw.points[0],observed_at:from+600}]}))
  assert.throws(()=>quotaHistory({...raw,points:[{...raw.points[0],hour:from+60}]}))
  assert.equal(quotaTimeline(account).hours.length,168,"older routers retain hourly display")
})
test("quota charts plot left, not used, including full and exhausted boundaries", () => {
  for (const [used,left,y] of [[0,100,20],[36,64,86.24],[77,23,161.68],[100,0,204],[125,0,204]]) {
    assert.equal(quotaLeft(used),left)
    assert.ok(Math.abs(quotaY(quotaLeft(used))-y)<1e-9)
  }
  const data=quotaTimeline(snapshot(doctor()).accounts[0])
  assert.equal(data.series[0].segments[0].split(" ").at(-1),"644,204")
  assert.equal(data.series[1].segments[0].split(" ").at(-1),"644,112")
})
test("usage series colors remain distinct and follow identity through selection and sorting", () => {
  const report=normalizeUsage(usage(),"model")
  const first=report.rows[0]
  const rows=Array.from({length:8},(_,i)=>({...first,key:`model-${i}`,name:`Model ${i}`}))
  const data={...report,rows,timeline:report.timeline.map((b)=>({...b,rows}))}
  const all=activity(data,"","tokens",Number.MAX_SAFE_INTEGER)
  assert.equal(new Set(all.series.map((s)=>s.color)).size,8)
  for (const line of all.series) {
    assert.equal(activity(data,line.key,"tokens",Number.MAX_SAFE_INTEGER).series[0].color,line.color)
    assert.equal(activity({...data,rows:[...rows].reverse()},line.key,"tokens",Number.MAX_SAFE_INTEGER).series[0].color,line.color)
  }
})
test("unknown usage is a gap, not an invented zero or a line across missing counts", () => {
  const report=normalizeUsage(usage(),"model"), row=report.rows[0]
  const buckets=Array.from({length:3},(_,i)=>({from:report.from+i*3600,to:report.from+(i+1)*3600,rows:[{...row,input:i===1?null:20,output:i===1?null:10}]}))
  const series=activity({...report,timeline:buckets},"","tokens",Number.MAX_SAFE_INTEGER).series[0]
  assert.deepEqual(series.values,[30,null,30])
  assert.equal(series.segments.length,0)
  assert.equal(series.isolated.length,2)
})
