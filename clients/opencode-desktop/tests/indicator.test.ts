import assert from "node:assert/strict"
import { test } from "node:test"
import { quotaIndicator } from "../src/indicator"
import { snapshot } from "../src/native"
import { doctor } from "./fixtures"
import type { DashboardState } from "../src/schemas"

test("quota indicator uses only the chat's actual account, never the Best recommendation", () => {
  const report = snapshot(doctor()), now = report.checkedAt
  const state: DashboardState = {revision:1,snapshot:report,error:null,refreshing:false,update:null,clipboardPending:false}
  const observation = { key: "a".repeat(64), model: "test-reasoner", observedAt: now }
  const indicator = (s: DashboardState | undefined, at = now) => quotaIndicator(s, at, observation)
  assert.equal(indicator(state)!.label,"8-hour 0%")
  const account = report.accounts[0], weekly = {...account.windows[0],minutes:10080,remaining:62}
  const actual = {...account,routing:{...account.routing,best:false},windows:[{...account.windows[0],remaining:81},weekly]}
  const recommended = {...account,id:2,observationKey:"b".repeat(64),label:"Another account"}
  const paired = {...state,snapshot:{...report,accounts:[recommended,actual]}}
  assert.equal(indicator(paired)!.label,"8-hour 81% · Weekly 62%")
  assert.equal(indicator({...state,snapshot:{...report,accounts:[{...account,windows:[weekly]}]}})!.label,"Weekly 62%")
  assert.equal(indicator(paired)!.id,account.id)
  assert.match(indicator(paired)!.title,/account used by this chat/)
  assert.doesNotMatch(indicator(paired)!.title,/recommend/)
  assert.equal(indicator({...state,refreshing:true})!.label,indicator(state)!.label)
  assert.match(indicator(state,now+60)!.title,/cached/)
  assert.equal(indicator({...state,error:"Unavailable"})!.id,account.id)
  assert.equal(indicator({...state,refreshing:true},now+600)!.label,indicator(state)!.label)
  assert.equal(quotaIndicator(state,now,null),undefined,"no Best fallback before a real primary response")
  assert.equal(quotaIndicator(state,now,{...observation,key:"c".repeat(64)}),undefined,"foreign profile/user must not match")
  assert.equal(indicator(undefined),undefined)
  assert.equal(indicator({...state,snapshot:{...report,accounts:[{...account,observationKey:null}]}}),undefined,"old servers cannot attribute the chat")
})
