import assert from "node:assert/strict"
import { test } from "node:test"
import { SessionAccounts, observationHeader } from "../src/session-account"

const a = "a".repeat(64), b = "b".repeat(64)
const request = { sessionID: "chat-a", kind: "primary", model: { providerID: "exetrouter", id: "synthetic" } }
const frame = (key: string) => JSON.stringify({ type: "response.metadata", headers: { [observationHeader]: key, authorization: "private-marker", "x-codex-turn-state": "private-marker" } })

test("actual primary WS and HTTP responses are isolated by session and follow failover", () => {
  const changed: string[] = [], accounts = new SessionAccounts((session) => changed.push(session))
  accounts.websocket({ ...request, frame: frame(a) })
  accounts.websocket({ ...request, sessionID: "chat-b", frame: frame(b) })
  assert.equal(accounts.get("chat-a")!.key,a)
  assert.equal(accounts.get("chat-b")!.key,b)
  for (const kind of ["title", "compaction", "generate"]) {
    accounts.websocket({ ...request, kind, frame: frame(b) })
    accounts.http({ ...request, kind, response: new Response(null,{headers:{[observationHeader]:b}}) })
  }
  accounts.http({ ...request, response:new Response(null,{status:429,headers:{[observationHeader]:b}}) })
  assert.equal(accounts.get("chat-a")!.key,a)
  accounts.websocket({ ...request, frame: frame(b) })
  assert.equal(accounts.get("chat-a")!.key,b,"same socket account transfer applies immediately")
  accounts.http({ ...request,response:new Response(null,{headers:{[observationHeader]:a}}) })
  assert.equal(accounts.get("chat-a")!.key,a)
  accounts.request({...request, model:{...request.model,providerID:"other"}})
  assert.equal(accounts.get("chat-a"),null)
  assert.equal(accounts.get("chat-b")!.key,b)
  assert.equal(changed.length,5)
  assert.doesNotMatch(JSON.stringify(accounts.get("chat-b")),/private-marker|authorization|turn-state/)
})

test("observation storage is bounded and malformed or absent metadata never guesses an account", () => {
  const accounts = new SessionAccounts(()=>{},2)
  for (const sessionID of ["one","two","three"]) accounts.websocket({...request,sessionID,frame:frame(a)})
  assert.equal(accounts.get("one"),null)
  for (const invalid of ["{",JSON.stringify({type:"response.completed",response:{text:"private-marker"}}),"x".repeat(65537)]) {
    accounts.websocket({...request,sessionID:"three",frame:invalid})
    assert.equal(accounts.get("three")!.key,a)
  }
  accounts.websocket({...request,sessionID:"three",frame:frame("invalid")})
  assert.equal(accounts.get("three"),null)
  accounts.http({...request,sessionID:"two",response:new Response()})
  assert.equal(accounts.get("two"),null)
})
