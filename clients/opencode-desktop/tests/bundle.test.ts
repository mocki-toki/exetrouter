import assert from "node:assert/strict"
import { isBuiltin, createRequire } from "node:module"
import { mkdtemp, writeFile, rm, readFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { setTimeout as delay } from "node:timers/promises"
import { test } from "node:test"
import { runInNewContext } from "node:vm"
import { Window } from "happy-dom"
import type { Plugin } from "@opencode/plugin/gui"
import { Rpc } from "@opencode/plugin/rpc"
import * as Effect from "effect"
import { doctor, usage } from "./fixtures"
import { modelList, tokenList } from "./fixtures"
import { snapshot, usage as normalizeUsage, models, tokens } from "../src/native"

const require = createRequire(import.meta.url)
// Mirrors the host's inert placeholders: module-scope Solid templates in main,
// Node imports in windows. Components themselves are not rendered by this smoke test.
const inert: unknown = new Proxy(function () {}, {
  get: () => inert, apply: () => inert, construct: () => inert as object,
})

async function bundle(main: boolean, runtime?: { solid: unknown; web: unknown; createLatest: unknown; usePlugin(): unknown }) {
  const code = await readFile(new URL("../dist/gui-experimental.cjs", import.meta.url), "utf8")
  const tokens = Object.fromEntries(["Command", "MenuItem", "Panel", "SettingsPage", "ComposerPill", "Style"].map((id) => [id, { id }]))
  const sdk = {
    ...tokens,
    Plugin: { define: (value: unknown) => value },
    Store: { global: (_schema: unknown, initial: unknown) => ({ initial }) },
    Ipc: { define: (spec: { id: string }) => ({ id: spec.id, spec }) },
    createLatest: runtime?.createLatest ?? (() => { throw new Error("View should only fetch when rendered.") }),
    createKeyed(source: () => unknown, fn: () => void) { if (runtime) (runtime.solid as typeof import("solid-js")).createEffect(() => { if (source() !== undefined) fn() }) },
    usePlugin: runtime?.usePlugin,
  }
  const exports: { default?: Plugin.Definition } = {}
  const timers = new Set<() => void>()
  runInNewContext(code, {
    exports,
    require(id: string) {
      if (id === "@opencode/plugin/gui") return sdk
      if (id === "solid-js") return runtime?.solid ?? inert
      if (id === "solid-js/web") return runtime?.web ?? inert
      if (isBuiltin(id)) return main ? require(id) : inert
      if (id === "effect") return Effect
      if (id === "@opencode/plugin/rpc") return { Rpc }
      throw new Error("Unexpected external module in GUI bundle.")
    },
    process: main ? process : undefined,
    Buffer: main ? Buffer : undefined,
    URL, TextEncoder, AbortSignal, AbortController, setTimeout, clearTimeout,
    setInterval(fn: () => void) { timers.add(fn); return fn },
    clearInterval(fn: () => void) { timers.delete(fn) },
  })
  assert.ok(exports.default)
  return { plugin: exports.default, timers }
}

test("the built GUI evaluates in a window with inert Node modules and registers only UI contributions", async () => {
  const { plugin } = await bundle(false)
  const contributions = new Map<string, unknown[]>()
  const opened: string[] = []
  const ctx = {
    id: "exetrouter", options: {},
    add(token: { id: string }, item: unknown) {
      contributions.set(token.id, [...(contributions.get(token.id) ?? []), typeof item === "function" ? item() : item])
      return () => {}
    },
    sessions: { current: () => undefined },
    layout: { open: (id: string) => opened.push(id) },
    main: () => ({ state: () => undefined }),
  }
  plugin.setup!(ctx as unknown as Parameters<Plugin.Setup>[0])
  assert.deepEqual([...contributions.keys()].sort(), ["Command", "MenuItem", "Panel", "Style", "ComposerPill"].sort())
  const command = contributions.get("Command")![0] as { run(): void }
  command.run()
  assert.deepEqual(opened, [])
  Object.assign(ctx.sessions, { current: () => ({ id: "synthetic-session" }) })
  command.run()
  assert.deepEqual(opened, ["exetrouter:dashboard"])
})

test("rendered panel has five native web views, account details, usage graph, tokens and models", async () => {
  const window = new Window()
  const solid = require("solid-js/dist/solid.cjs") as typeof import("solid-js")
  const webExports: Record<string, unknown> = {}
  runInNewContext(await readFile(require.resolve("solid-js/web/dist/web.cjs"), "utf8"), {
    exports: webExports, require: (id: string) => { assert.equal(id, "solid-js"); return solid },
    window, document: window.document, Node: window.Node, Element: window.Element,
  })
  const web = webExports as typeof import("solid-js/web")
  let host: unknown
  const runtime = { solid, web, usePlugin: () => host, createLatest(source: () => unknown, fetch: (input: unknown, signal: AbortSignal) => Promise<unknown>) {
    const [latest, setLatest] = solid.createSignal<unknown>()
    const [error, setError] = solid.createSignal<unknown>()
    const [loading, setLoading] = solid.createSignal(true)
    solid.createEffect(() => {
      const abort = new AbortController()
      solid.onCleanup(() => abort.abort())
      setLoading(true)
      void fetch(source(), abort.signal).then((value) => { if (!abort.signal.aborted) { setLatest(() => value); setLoading(false) } }, (err) => { setError(err); setLoading(false) })
    })
    return { get latest() { return latest() }, get loading() { return loading() }, get error() { return error() } }
  } }
  const { plugin, timers } = await bundle(false, runtime)
  const contributions = new Map<string, unknown>()
  const report = snapshot(doctor())
  const rawUsage = usage()
  rawUsage.timeline[0].from_utc = Math.floor(Date.now() / 1000) - 3600
  const usageReport = normalizeUsage(rawUsage, "total")
  const copied: string[] = []
  const calls = { usage: 0, tokens: 0, models: 0, updates: 0 }
  const [state, setState] = solid.createSignal({ revision:0, snapshot: report, error: null as string | null, refreshing: false, update: null as null | {current:string;latest:string;available:boolean}, clipboardPending: false })
  const [available, setAvailable] = solid.createSignal(true)
  const nativeActions: Record<string, unknown>[] = []
  let tokenRows = tokens(tokenList())
  let tokenGate: ReturnType<typeof import("./fixtures").deferred<typeof tokenRows>> | undefined
  const bridge = {
    state:()=>available()?state():undefined, refresh: async () => {setState((old)=>({...old,revision:old.revision+1}))},
    usage: async () => { calls.usage++; return usageReport }, tokens: async () => { calls.tokens++; return tokenGate ? tokenGate.promise : tokenRows }, models: async () => { calls.models++; return models(modelList()) },
    profile: async () => ({ mode: "Standalone", destination: "127.0.0.1:8787", fields: [{ label: "SSH host", value: "router.example.com" }], accounts: [{id:1,email:"account@example.com",state:"active"}], clientVersion: "0.0.0-test", connection: {mode:"standalone",host:"router.example.com",port:2222,ssh_user:"routercli",identity:"/synthetic/key",api_url:null,state_dir:"/synthetic/state",listen:"127.0.0.1:8787"} }),
    desktop: async (input: Record<string, unknown>) => { nativeActions.push(input); return {status:"saved",copied:input.action === "token_create" || input.action === "token_rotate",retryCopy:false,profile:null,update:null,login:input.action === "login_start" ? {url:"https://auth.openai.com/activate",code:"TEST-CODE"} : null} },
    update: async () => { calls.updates++; return { current: "0.0.0-test", latest: "0.0.0-test", available: false } },
  }
  type UI = Parameters<Plugin.Setup>[0]["ui"]
  const button = (props: Parameters<UI["Button"]>[0] & { label?: string }) => {
    const b = window.document.createElement("button")
    web.insert(b as unknown as HTMLElement, () => props.children ?? props.label)
    solid.createEffect(() => { b.disabled = !!props.busy || !!props.disabled; if (props.label) b.setAttribute("aria-label", props.label) })
    b.onclick = props.onClick as (() => void) | undefined ?? null
    return b
  }
  const ui = {
    Button: button,
    IconButton: (props: Parameters<UI["IconButton"]>[0]) => button({ get children() { return props.label }, get label() { return props.label }, get busy() { return props.busy }, onClick: props.onClick }),
    Icon: () => window.document.createElement("span"),
    Tooltip: (props: Parameters<UI["Tooltip"]>[0]) => props.children,
    Loader: () => { const el = window.document.createElement("span"); el.setAttribute("role", "progressbar"); return el },
    Code: (props: Parameters<UI["Code"]>[0]) => { const el = window.document.createElement("pre"); el.textContent = props.content; return el },
    TextInput: (props: Parameters<UI["TextInput"]>[0]) => {
      // Match the host's wrapper/slot structure, not just a bare input.
      const wrapper = window.document.createElement("div")
      wrapper.setAttribute("data-component", "text-input-v2")
      const value = window.document.createElement("div")
      value.setAttribute("data-slot", "text-input-v2-value")
      const el = window.document.createElement("input")
      el.setAttribute("aria-label", props.label)
      el.setAttribute("data-slot", "text-input-v2-input")
      solid.createEffect(() => { el.value = props.value })
      el.oninput = () => props.onInput(el.value)
      value.append(el); wrapper.append(value)
      return wrapper
    },
    Menu: (props: Parameters<UI["Menu"]>[0]) => {
      const el = window.document.createElement("div")
      const trigger = window.document.createElement("button")
      solid.createEffect(() => { trigger.textContent = props.label })
      trigger.onclick = () => {
        const choices = window.document.createElement("div")
        choices.setAttribute("role", "menu")
        for (const item of props.items) {
          if (item.type === "separator" || item.type === "group") continue
          const choice = window.document.createElement("button")
          choice.textContent = item.label; choice.setAttribute("role", "menuitem")
          choice.disabled = !!item.disabled
          choice.onclick = () => { item.onSelect(); choices.remove() }
          choices.append(choice)
        }
        el.append(choices)
      }
      el.append(trigger)
      return el
    },
    Dialog: (props: Parameters<UI["Dialog"]>[0]) => {
      const el = window.document.createElement("div")
      el.setAttribute("role", "dialog"); el.setAttribute("aria-label", props.title)
      const description = window.document.createElement("p")
      description.textContent = props.description ?? ""
      el.append(description)
      web.insert(el as unknown as HTMLElement, () => props.children)
      web.insert(el as unknown as HTMLElement, () => props.footer)
      return el
    },
  }
  const dialogs = new Set<() => void>()
  const [preferences, setPreferences] = solid.createSignal({tab:"Overview",period:"day",by:"total",metric:"tokens"})
  let accountChanged: ((event: {location:{directory:string};data:{sessionID:string;account:unknown}}) => void) | undefined
  const session = { id:"chat-a",directory:"/synthetic",server:{ id:"local",builtin:true,connected:true,client:{rpc:()=>({events:{on(_type:string,fn:typeof accountChanged){accountChanged=fn;return()=>{}}},get:async()=>({key:"a".repeat(64),model:"synthetic",observedAt:report.checkedAt})})} } }
  const ctx = {
    id: "exetrouter", options: {}, signal: new AbortController().signal,
    add: (token: { id: string }, item: unknown) => { contributions.set(token.id, item); return () => {} },
    sessions: { current: () => session }, main: () => bridge,
    ui,
    stores: { preferences: { get value() { return preferences() }, ready:()=>true, update(fn:(draft:ReturnType<typeof preferences>)=>void) {const next={...preferences()};fn(next);setPreferences(next)} } },
    dialogs: { open(render: (dialog: { close(): void }) => unknown) {
      const el = window.document.createElement("div"); window.document.body.append(el)
      const close = () => { dispose(); el.remove(); dialogs.delete(close) }
      const dispose = web.render(() => render({ close }) as HTMLElement, el as unknown as HTMLElement)
      dialogs.add(close)
      // Match the real host: a dialog closes when its opening owner ends.
      solid.onCleanup(close)
      return { close }
    } },
    system: { copy: async (value: string) => { copied.push(value) } }, toast: { show: () => {} },
  }
  host = ctx
  const root = window.document.createElement("div")
  window.document.body.append(root)
  let dispose: (() => void) | undefined
  try {
    solid.createRoot(() => { plugin.setup!(ctx as unknown as Parameters<Plugin.Setup>[0]) })
    const panel = contributions.get("Panel") as { render(input: unknown): HTMLElement }
    dispose = web.render(() => panel.render({ session: { server: { builtin: true } } }), root as unknown as HTMLElement)
    await delay(10)
    assert.deepEqual([...root.querySelectorAll('[role="tab"]')].map((e) => e.textContent), ["Overview", "Usage", "Tokens", "Models", "Settings"])
    assert.doesNotMatch(root.textContent, /3 active accounts/)
    assert.match(root.textContent, /ExetRouter v0.0.0-test ready/)
    assert.doesNotMatch(root.textContent, /OAuth|require login/)
    assert.doesNotMatch(root.textContent, /Passive refresh every|GMT/)
    assert.match(root.textContent, /Synthetic account/)
    assert.match(root.textContent, /2 resets/)
    assert.deepEqual(calls, { usage: 0, tokens: 0, models: 0, updates: 0 })
    const click = (label: string) => [...window.document.querySelectorAll("button")].find((e) => e.getAttribute("aria-label") === label || e.textContent === label)!.click()
    const selectTab = async (name: string) => {
      const tab = [...root.querySelectorAll('[role="tab"]')].find((e) => e.textContent === name)!
      tab.dispatchEvent(new window.MouseEvent("click", { bubbles: true })); await delay(10)
      assert.equal(root.querySelector('[role="tabpanel"]:not([hidden])')?.getAttribute("aria-labelledby"), tab.id)
    }
    click("View Synthetic account details")
    assert.match(window.document.querySelector('[role="dialog"]')!.textContent, /Synthetic client/)
    const accountDialog = window.document.querySelector('[role="dialog"]')!
    const history = accountDialog.querySelector('svg[aria-label^="Hourly subscription"]')!
    assert.ok(history)
    assert.equal(history.querySelectorAll("[data-window]").length,2,"both windows share the same chart")
    history.dispatchEvent(new window.KeyboardEvent("keydown",{key:"End",bubbles:true}))
    assert.match(accountDialog.querySelector('[role="status"]')!.textContent,/8-hour.*100% used.*Weekly.*50% used/)
    history.dispatchEvent(new window.KeyboardEvent("keydown",{key:"Home",bubbles:true}))
    assert.match(accountDialog.querySelector('[role="status"]')!.textContent,/Not observed/)
    const accountPane = root.querySelector('[role="tabpanel"]')!
    accountPane.scrollTop = 90
    setState((old) => ({ ...old, refreshing: true }))
    assert.equal(window.document.querySelector('[role="dialog"]'), accountDialog)
    setAvailable(false)
    assert.equal(window.document.querySelector('[role="dialog"]'), accountDialog)
    assert.match(accountDialog.textContent,/Synthetic client/)
    assert.equal(root.querySelector('[role="tabpanel"]'),accountPane)
    setAvailable(true)
    setState((old) => ({ ...old, refreshing:false, snapshot:{...old.snapshot, accounts:old.snapshot.accounts.map((a) => ({...a,cooldownUntil:a.windows[0].resetAt,cooldownSource:"upstream_reset",clients:{...a.clients,rows:a.clients.rows.map((c)=>({...c,name:"Updated client"}))}}))} }))
    assert.equal(window.document.querySelector('[role="dialog"]'), accountDialog)
    assert.match(accountDialog.textContent, /Updated client/)
    assert.equal(accountDialog.querySelector('svg[aria-label^="Hourly subscription"]'),history,"background refresh preserves the chart and cursor owner")
    assert.match(accountDialog.querySelector('[role="status"]')!.textContent,/Not observed/)
    assert.equal(root.querySelector('[role="tabpanel"]'), accountPane)
    assert.equal(accountPane.scrollTop, 90)
    assert.match(root.querySelector('.exr-quota-pause')!.textContent,/Paused until/)
    assert.equal(root.querySelector('.exr-quota-area')!.hasAttribute("data-paused"),true)
    assert.doesNotMatch(root.querySelector('.exr-account-header')!.textContent,/Paused until/)
    assert.equal([...root.querySelectorAll('.exr-notice')].some((n)=>n.textContent.includes("Paused until")),false)
    click("Done")
    await selectTab("Tokens")
    click("Create token")
    const setInput = (label: string, value: string) => {
      const input = window.document.querySelector(`input[aria-label="${label}"]`)!
      ;(input as unknown as {value:string}).value = value
      input.dispatchEvent(new window.Event("input", {bubbles:true}))
    }
    setInput("Token name", "New synthetic token")
    click("Create and copy"); await delay(10)
    assert.equal(nativeActions.at(-1)?.action,"token_create")
    assert.equal(nativeActions.at(-1)?.days,90)
    click("Actions for Synthetic token"); click("Rename token")
    const renameDialog = window.document.querySelector('[role="dialog"]')!
    setInput("Token name", "Unsubmitted draft")
    const { deferred } = await import("./fixtures")
    tokenGate = deferred<typeof tokenRows>()
    click("Refresh router snapshot"); await delay(10)
    assert.equal(window.document.querySelector('[role="dialog"]'), renameDialog)
    assert.match(root.textContent, /Synthetic token/)
    tokenRows = tokenRows.map((t) => ({...t,name:"Refreshed token"}))
    tokenGate.resolve(tokenRows); tokenGate = undefined
    await delay(10)
    assert.equal(window.document.querySelector('[role="dialog"]'), renameDialog)
    assert.equal((window.document.querySelector('input[aria-label="Token name"]') as unknown as {value:string}).value,"Unsubmitted draft")
    click("Save name"); await delay(10)
    assert.equal(nativeActions.at(-1)?.name,"Unsubmitted draft")
    click("Actions for Refreshed token"); click("Rotate token")
    assert.equal(window.document.querySelector('[role="dialog"]')!.textContent.match(/Replace Refreshed token/g)?.length,1)
    assert.notEqual(nativeActions.at(-1)?.action,"token_rotate")
    click("Rotate and copy"); await delay(10)
    assert.equal(nativeActions.at(-1)?.action,"token_rotate")
    assert.equal(nativeActions.at(-1)?.confirmed,true)
    click("Actions for Refreshed token"); click("Revoke token"); click("Revoke token"); await delay(10)
    assert.equal(nativeActions.at(-1)?.action,"token_revoke")
    await selectTab("Usage")
    assert.doesNotMatch(root.textContent,/About these numbers|Shared pool totals|incomplete|Unknown:|partial:/i)
    assert.ok(root.querySelector('.exr-compact-stats'))
    assert.ok(root.querySelector("svg"))
    const chart = root.querySelector('svg[role="img"]')!
    Object.defineProperty(chart,"getBoundingClientRect",{value:()=>({left:0,width:668})})
    chart.dispatchEvent(new window.MouseEvent("pointermove",{clientX:44,bubbles:true}))
    assert.ok(root.querySelector('[data-slot="activity-cursor"]'))
    assert.match(root.querySelector('.exr-chart-tooltip')!.textContent,/tokens/)
    assert.doesNotMatch(root.querySelector('.exr-chart-tooltip')!.textContent,/incomplete| – /)
    assert.match(root.querySelector('.exr-chart-tooltip')!.getAttribute("style")!,/24px/)
    chart.dispatchEvent(new window.MouseEvent("pointerleave",{bubbles:true}))
    assert.equal(root.querySelector('[data-slot="activity-cursor"]'),null)
    chart.dispatchEvent(new window.KeyboardEvent("keydown",{key:"End",bubbles:true}))
    assert.ok(root.querySelector('[data-slot="activity-cursor"]'))
    assert.match(root.textContent, /Input/)
    assert.match(root.textContent, /Unknown/)
    click("24 hours"); await delay(10)
    assert.equal(calls.usage, 2)
    click("Group: Total")
    click("Token"); await delay(10)
    assert.match(root.textContent, /Only your API tokens are included/)
    assert.equal(calls.usage, 3)
    await selectTab("Tokens")
    assert.match(root.textContent, /Refreshed token/)
    assert.ok(root.querySelector('[data-component="exr-settings-list"] [data-slot="row-control"]'))
    click("View Refreshed token details")
    assert.match(window.document.querySelector('[role="dialog"]')!.textContent, /Created/)
    click("Done")
    await selectTab("Models")
    assert.match(root.textContent, /synthetic-model/)
    assert.match(root.textContent, /100,000/)
    click("Copy synthetic-model ID"); await delay(10)
    assert.deepEqual(copied, ["synthetic-model"])
    const search = root.querySelector('input[aria-label="Search models"]')! as unknown as { value: string; dispatchEvent(event: unknown): void }
    search.value = "no matching model"
    search.dispatchEvent(new window.Event("input", { bubbles: true }))
    assert.match(root.textContent, /No matching models/)
    await selectTab("Settings")
    assert.equal(root.querySelector('[role="tabpanel"]:not([hidden]) h3')!.textContent,"Client updates")
    assert.match(root.textContent, /router.example.com/)
    assert.match(root.textContent, /Not checked/)
    assert.equal(calls.updates, 0)
    setState((old)=>({...old,update:{current:"0.0.0-test",latest:"1.0.0",available:true}}))
    assert.ok(root.querySelector('.exr-update[data-available]'))
    click("Configure")
    setInput("Private state directory","/synthetic/new-state")
    setState((old)=>({...old,revision:old.revision+1,snapshot:{...old.snapshot,checkedAt:old.snapshot.checkedAt+30}})); await delay(10)
    assert.equal((window.document.querySelector('input[aria-label="Private state directory"]') as unknown as {value:string}).value,"/synthetic/new-state")
    click("Save connection"); await delay(10)
    assert.equal(nativeActions.at(-1)?.action,"configure")
    click("Actions for account@example.com"); click("Disable account")
    assert.notEqual(nativeActions.at(-1)?.action,"account_disable")
    click("Disable account"); await delay(10)
    assert.equal(nativeActions.at(-1)?.action,"account_disable")
    click("Add account"); await delay(10)
    assert.match(window.document.querySelector('[role="dialog"]')!.textContent,/TEST-CODE/)
    click("Cancel sign-in"); await delay(10)
    assert.equal(nativeActions.at(-1)?.action,"login_cancel")
    await selectTab("Usage")
    const usagePane = root.querySelector('[role="tabpanel"]:not([hidden])')!
    assert.equal([...usagePane.querySelectorAll('[aria-pressed="true"]')].some((e) => e.textContent === "24 hours"), true)
    assert.equal(calls.usage, 4, "a newer snapshot refreshes visible usage without resetting filters")
    assert.equal(preferences().period,"24h")
    assert.equal(preferences().by,"token")
    await selectTab("Models")
    assert.equal((root.querySelector('input[aria-label="Search models"]') as unknown as { value: string }).value, "no matching model")
    assert.equal(calls.models, 2)
    search.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }))
    assert.equal(search.value, "")
    assert.equal(window.document.activeElement, search)
    assert.match(root.textContent, /synthetic-model/)
    search.value = "missing"
    search.dispatchEvent(new window.Event("input", { bubbles: true }))
    click("Clear search models")
    assert.equal(search.value, "")
    assert.equal(window.document.activeElement, search)
    assert.equal(calls.models, 2, "clearing a local filter must not refetch metadata")
    await selectTab("Settings")
    assert.equal(contributions.has("ComposerPill"), true)
    assert.equal(contributions.has("SettingsPage"), false)
    root.querySelector('[role="tablist"]')!.dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }))
    await delay(10)
    assert.equal(root.querySelector('[role="tab"][aria-selected="true"]')?.textContent, "Overview")
    assert.equal([...root.querySelectorAll('button[aria-label^="Refresh"]')].length,1)
    const observed = Math.floor(Date.now()/1000)
    setState((old)=>({...old,snapshot:{...old.snapshot,checkedAt:observed,accounts:old.snapshot.accounts.map((a)=>({...a,routing:{...a.routing,best:true,observedAt:observed,validUntil:observed+60},windows:a.windows.map((w)=>({...w,remaining:75,used:25,observedAt:observed,resetAt:observed+3600}))}))}}))
    for (const tick of timers) tick()
    assert.equal(root.querySelector('.exr-best')!.textContent,"Best")
    assert.equal(root.querySelector('.exr-best')!.classList.contains("exr-badge"),false)
    const indicator = contributions.get("ComposerPill") as {show(input:unknown):{label:string;run():void}}
    const pill = indicator.show({session,screen:{}})
    assert.match(pill.label,/8-hour 75%/)
    setAvailable(false)
    assert.equal(indicator.show({session,screen:{}}).label,pill.label)
    assert.equal(root.querySelector('.exr-best')!.textContent,"Best")
    setAvailable(true)
    pill.run()
    assert.match(window.document.querySelector('[role="dialog"]')!.textContent,/Updated client/)
    click("Done")
    const oldObserved=observed-120
    setState((old)=>({...old,refreshing:true,snapshot:{...old.snapshot,checkedAt:oldObserved,accounts:old.snapshot.accounts.map((a)=>({...a,routing:{...a.routing,observedAt:oldObserved,validUntil:oldObserved+60},windows:a.windows.map((w)=>({...w,observedAt:oldObserved}))}))}}))
    assert.equal(root.querySelector('.exr-best')!.textContent,"Best")
    assert.match(root.querySelector('.exr-best')!.getAttribute("title")!,/cached/)
    assert.equal(indicator.show({session,screen:{}}).label,pill.label)
    accountChanged!({location:{directory:"/different"},data:{sessionID:session.id,account:null}})
    assert.equal(indicator.show({session,screen:{}}).label,pill.label,"ignore events from another location")
    accountChanged!({location:{directory:session.directory},data:{sessionID:session.id,account:null}})
    assert.equal(indicator.show({session,screen:{}}),undefined,"provider change must not fall back to Best")
  } finally { for (const close of dialogs) close(); dispose?.(); await window.happyDOM.close() }
})

test("the built main entry polls a synthetic exr once for all windows, publishes state, and cleans up", async () => {
  const root = await mkdtemp(join(tmpdir(), "exetrouter-gui-main-"))
  const controller = new AbortController()
  let cleanup: (() => void | Promise<void>) | void = undefined
  try {
    const binary = join(root, "exr-fixture")
    await writeFile(binary, `#!/usr/bin/env node\nprocess.stdout.write(JSON.stringify(process.argv.includes("doctor") ? ${JSON.stringify(doctor())} : ${JSON.stringify(usage())}));\n`, { mode: 0o700 })
    const { plugin, timers } = await bundle(true)
    let impl: { state(): { snapshot: { version: string } | null }; refresh(): Promise<void> } | undefined
    const ctx = {
      options: { exrBinary: binary }, signal: controller.signal,
      provide(_token: unknown, implementation: typeof impl) { impl = implementation; return { changed() {} } },
    }
    cleanup = await plugin.main!(ctx as unknown as Parameters<Plugin.MainSetup>[0])
    assert.equal(timers.size, 1)
    for (let i = 0; i < 100 && !impl?.state().snapshot; i++) await delay(10)
    assert.equal(impl?.state().snapshot?.version, "0.0.0-test")
    const first = impl!.refresh()
    assert.equal(impl!.refresh(), first)
    await first
    controller.abort()
    await cleanup?.()
    cleanup = undefined
    assert.equal(timers.size, 0)
  } finally {
    controller.abort()
    await cleanup?.()
    await rm(root, { recursive: true, force: true })
  }
})
