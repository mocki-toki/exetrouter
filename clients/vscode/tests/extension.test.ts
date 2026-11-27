import assert = require("node:assert/strict")
import { test } from "node:test"
import { readFileSync } from "node:fs"
import { createServer } from "node:http"
import { AddressInfo } from "node:net"
import { once } from "node:events"
import { EventEmitter as NodeEmitter } from "node:events"
import { PassThrough, Writable } from "node:stream"
import type * as vscode from "vscode"
import { secretKey } from "../src/catalog"

// Exercise the packed entrypoint and public API adapter with synthetic host services.
// Never inherit a real exr, user profile, extension storage, or external endpoint.
test("bundled host registers overview, usage, tokens and models and scopes chat credentials", async (t) => {
  const manifests = JSON.parse(readFileSync("package.json", "utf8"))
  const requests: Record<string, any>[] = []
  const api = createServer(async (req, res) => {
    if (req.headers.authorization !== "Bearer synthetic") {
      res.writeHead(401)
      res.end()
      return
    }
    if (req.url === "/first/v1/models") {
      res.end(
        JSON.stringify({
          data: [
            {
              id: "fixture",
              display_name: "Fixture",
              capabilities: ["tools"],
              exetrouter: {
                context_window: 1000,
                input_modalities: ["text"],
                supported_reasoning_levels: [{ effort: "medium" }],
              },
            },
          ],
        }),
      )
      return
    }
    if (req.url !== "/first/v1/responses") {
      res.writeHead(404)
      res.end()
      return
    }
    const chunks = []
    for await (const chunk of req) chunks.push(chunk)
    requests.push(JSON.parse(Buffer.concat(chunks).toString()))
    res.writeHead(200, { "content-type": "text/event-stream" })
    res.end(
      `data: ${JSON.stringify({ type: "response.completed", response: { output: [{ type: "function_call", id: "item", call_id: "call", name: "find", arguments: "{}" }] } })}\n\n`,
    )
  })
  api.listen(0, "127.0.0.1")
  await once(api, "listening")
  t.after(() => {
    api.closeAllConnections()
    api.close()
  })
  const origin = `http://127.0.0.1:${(api.address() as AddressInfo).port}`
  let url = `${origin}/first/v1`,
    configChange: ((event: unknown) => unknown) | undefined
  const keys: string[] = [],
    copied: string[] = [],
    views: string[] = [],
    commands = new Map<string, () => unknown>()
  const subscriptions: { dispose: () => void }[] = []
  t.after(() => subscriptions.forEach((o) => o.dispose()))
  const disposable = { dispose() {} }
  class EventEmitter {
    event = () => disposable
    fire() {}
    dispose() {}
  }
  class Text {
    constructor(public value: string) {}
  }
  class Call {
    constructor(
      public callId: string,
      public name: string,
      public input: object,
    ) {}
  }
  class Result {
    constructor(
      public callId: string,
      public content: Text[],
    ) {}
  }
  class Data {}
  let provider!: vscode.LanguageModelChatProvider
  const host = {
    env: {
      clipboard: {
        writeText: async (s: string) => {
          copied.push(s)
        },
      },
    },
    EventEmitter,
    CancellationError: class extends Error {},
    StatusBarAlignment: { Right: 1 },
    LanguageModelChatMessageRole: { User: 1, Assistant: 2 },
    LanguageModelChatToolMode: { Required: 2 },
    LanguageModelTextPart: Text,
    LanguageModelToolCallPart: Call,
    LanguageModelToolResultPart: Result,
    LanguageModelDataPart: Data,
    workspace: {
      getConfiguration: () => ({
        get: (key: string) => ({ apiBaseUrl: url, reasoningEffort: "medium", executable: "exr" })[key],
      }),
      onDidChangeConfiguration: (cb: typeof configChange) => {
        configChange = cb
        return disposable
      },
    },
    lm: {
      registerLanguageModelChatProvider: (vendor: string, p: typeof provider) => {
        assert.equal(vendor, "exetrouter")
        provider = p
        return disposable
      },
    },
    window: {
      showQuickPick: async (items: unknown[]) => items[0],
      createStatusBarItem: () => ({ ...disposable, show() {} }),
      registerTreeDataProvider: (id: string) => {
        views.push(id)
        return disposable
      },
      registerWebviewViewProvider: (id: string) => {
        views.push(id)
        return disposable
      },
    },
    commands: {
      registerCommand: (id: string, cb: () => unknown) => {
        commands.set(id, cb)
        return disposable
      },
    },
  }
  const mod = { exports: {} as { activate: (ctx: unknown) => void } }
  const load = (id: string) =>
    id === "vscode"
      ? host
      : id === "node:fs"
        ? { statSync: () => undefined }
        : id === "node:child_process"
          ? {
              spawn() {
                throw Error("Native bridge unavailable")
              },
              execFile() {
                throw Error("Native reports unavailable")
              },
            }
          : require(id)
  new Function("require", "module", "exports", readFileSync("dist/extension.js", "utf8"))(
    load,
    mod,
    mod.exports,
  )
  mod.exports.activate({
    subscriptions,
    secrets: {
      get: async (k: string) => {
        keys.push(k)
        return k === secretKey(`${origin}/first/v1`) ? "synthetic" : undefined
      },
      onDidChange: () => disposable,
    },
  })
  assert.deepEqual(
    views.sort(),
    manifests.contributes.views.exetrouter.map((v: { id: string }) => v.id).sort(),
  )
  const token = { isCancellationRequested: false, onCancellationRequested: () => disposable }
  const infos = await provider.provideLanguageModelChatInformation({ silent: true }, token)
  assert.equal(infos?.length, 1)
  const info = infos![0]
  await commands.get("exetrouter.copyModel")!()
  assert.deepEqual(copied, ["fixture"])
  const messages = [
    { role: 3, content: [new Text("Use tools")], name: undefined },
    { role: 1, content: [new Text("Find it")], name: undefined },
    { role: 2, content: [new Call("previous", "find", {})], name: undefined },
    { role: 1, content: [new Result("previous", [new Text("the result")])], name: undefined },
  ]
  const parts: unknown[] = []
  await provider.provideLanguageModelChatResponse(
    info,
    messages,
    { toolMode: 2, tools: [{ name: "find", description: "Search" }] },
    { report: (p) => parts.push(p) },
    token,
  )
  assert.deepEqual(parts, [new Call("call", "find", {})])
  assert.equal(requests[0].tool_choice, "required")
  assert.equal(requests[0].max_output_tokens, undefined)
  assert.deepEqual(requests[0].reasoning, { effort: "medium" })
  assert.equal(requests[0].input[0].role, "developer")
  assert.equal(requests[0].input[3].call_id, "previous")
  assert.equal(requests[0].input[3].output, "the result")
  assert.ok((await provider.provideTokenCount(info, "synthetic <|endoftext|>", token)) > 0)
  url = `${origin}/second/v1`
  configChange!({ affectsConfiguration: (s: string) => s === "exetrouter" })
  assert.deepEqual(await provider.provideLanguageModelChatInformation({ silent: true }, token), [])
  assert.equal(keys.at(-1), secretKey(url))
  await assert.rejects(
    Promise.resolve(
      provider.provideLanguageModelChatResponse(info, messages, { toolMode: 1 }, { report: () => {} }, token),
    ),
    /connection changed/,
  )
  assert.equal(requests.length, 1)
})

test("dashboard retains stale reports only within profile and confirms native mutations", async (t) => {
  let fail = false,
    stamp = 1n,
    confirmed = false,
    address = ""
  const actions: Record<string, unknown>[] = [],
    choices: string[] = [],
    warnings: string[] = [],
    inputs: string[] = []
  const args: string[][] = [],
    registered = new Map<string, (id?: unknown) => Promise<unknown>>()
  const saved = new Map<string, string>()
  const views = new Map<
    string,
    { getChildren: () => Promise<Array<{ label: string; description?: string }>> }
  >()
  const subscriptions: { dispose: () => void }[] = []
  t.after(() => subscriptions.forEach((o) => o.dispose()))
  const noop = { dispose() {} }
  const status = { ...noop, text: "", tooltip: "", show() {} }
  const charts: ReturnType<typeof webview>[] = []
  let resolveUsage: ((view: ReturnType<typeof webview>) => void) | undefined
  function webview() {
    let receive: ((value: unknown) => void) | undefined
    const events = new NodeEmitter()
    const sent: Record<string, any>[] = []
    return {
      sent,
      webview: {
        cspSource: "synthetic-resource",
        asWebviewUri: (uri: string) => uri,
        onDidReceiveMessage(fn: typeof receive) { receive = fn; return noop },
        postMessage(value: Record<string, unknown>) { sent.push(value); return Promise.resolve(true) },
      },
      onDidDispose(fn: () => void) { events.on("dispose", fn); return noop },
      dispose() { events.emit("dispose") },
      message(value: unknown) { receive?.(value) },
    }
  }
  const settle = () => new Promise<void>((resolve) => setImmediate(resolve))
  const doctor = {
    schema_version: 1,
    quota_accounts: [
      { id: 7, label: "First", quota: { status: "current" } },
      { id: 9, label: "Second", quota: { status: "cooldown" } },
    ],
  }
  class EventEmitter {
    event = () => noop
    fire() {}
    dispose() {}
  }
  const host = {
    EventEmitter,
    StatusBarAlignment: { Right: 1 },
    ViewColumn: { Active: 1 },
    ConfigurationTarget: { Global: 1 },
    Uri: { joinPath: (...paths: string[]) => paths.join("/") },
    workspace: {
      getConfiguration: () => ({
        get: (key: string) => key === "apiBaseUrl" ? address : "",
        update: async (key: string, value: string) => { assert.equal(key, "apiBaseUrl"); address = value },
      }),
      onDidChangeConfiguration: () => noop,
    },
    lm: { registerLanguageModelChatProvider: () => noop },
    commands: {
      registerCommand: (name: string, cb: (id?: unknown) => Promise<unknown>) => {
        registered.set(name, cb)
        return noop
      },
    },
    window: {
      createStatusBarItem: () => status,
      registerTreeDataProvider: (a: string, v: never) => {
        views.set(a, v)
        return noop
      },
      registerWebviewViewProvider: (_: string, p: { resolveWebviewView: typeof resolveUsage }) => {
        resolveUsage = p.resolveWebviewView
        return noop
      },
      createWebviewPanel() {
        const panel = webview()
        charts.push(panel)
        return panel
      },
      showQuickPick: async (list: unknown[]) => {
        const choice = choices.shift()
        assert.ok(choice)
        return list.find((i) => i === choice || (i as { label: string }).label === choice)
      },
      showInputBox: async () => inputs.shift() ?? "New label",
      showWarningMessage: async (msg: string, opts: unknown, yes: string) => {
        warnings.push(msg)
        return confirmed ? yes : undefined
      },
      showInformationMessage: async () => {},
    },
  }
  const cp = {
    spawn() {
      const out = new PassThrough(),
        child = Object.assign(new NodeEmitter(), {
          stdin: new Writable({
            write(chunk, _, cb) {
              const { id, input } = JSON.parse(chunk.toString())
              actions.push(input)
              const value =
                input.action === "profile"
                  ? { profile: { mode: "Remote Server", destination: "synthetic target" } }
                  : { status: "saved" }
              out.write(JSON.stringify({ id, value }) + "\n")
              cb()
            },
          }),
          stdout: out,
          kill() {},
        })
      return child
    },
    execFile(_: string, a: string[], _o: unknown, cb: (err: unknown, out: string) => void) {
      args.push(a)
      queueMicrotask(() =>
        cb(
          fail ? new Error("fixture unavailable") : null,
          JSON.stringify(
            a[0] === "doctor"
              ? doctor
              : a[0] === "token"
                ? [{ id: "tok_fixture", name: "Old label" }]
                : a[0] === "quota-history"
                  ? {
                      from_utc: 1700000000, to_utc: 1700000600,
                      interval_seconds: 600,
                      points: [{ kind: "primary", used_percent: 32, hour: 1700000000 }],
                    }
                  : { from_utc: 1700000000, to_utc: 1700000600, rows: [{ name: "Fixture model", requests: 1, total_tokens: 5 }], timeline: [{ from_utc: 1700000000, to_utc: 1700000600, rows: [{ name: "Fixture model", requests: 1, total_tokens: 5 }] }] },
          ),
        ),
      )
    },
  }
  const mod = { exports: {} as { activate: (ctx: unknown) => void } }
  new Function("require", "module", "exports", readFileSync("dist/extension.js", "utf8"))(
    (id: string) =>
      id === "vscode"
        ? host
        : id === "node:child_process"
          ? cp
          : id === "node:fs"
            ? { statSync: () => ({ dev: 1n, ino: stamp, mtimeNs: 1n, size: 1n }) }
            : require(id),
    mod,
    mod.exports,
  )
  mod.exports.activate({ extensionUri: "extension", subscriptions, secrets: {
    onDidChange: () => noop,
    store: async (key: string, value: string) => saved.set(key, value),
  } })
  const run = (s: string, arg?: unknown) => registered.get(`exetrouter.${s}`)!(arg),
    rows = (s: string) => views.get(`exetrouter.${s}`)!.getChildren()
  await run("refresh")
  assert.match((await rows("models"))[0].label, /API address is missing/)
  const usage = webview(), selectUsage = (group: string) => usage.message({ type: "chart", period: "7d", metric: "tokens", group })
  resolveUsage!(usage)
  selectUsage("model")
  await settle()
  assert.equal(usage.sent.at(-1)?.chart.series[0]?.label, "Fixture model")
  await rows("tokens")
  assert.match(status.tooltip, /First.*\nSecond/)
  choices.push("Rename")
  await run("manageToken", "tok_fixture")
  assert.equal(actions.filter((a) => a.action !== "profile").length, 0)
  assert.match(warnings.at(-1)!, /New label/)
  confirmed = true
  choices.push("Pause")
  await run("manageToken", "tok_fixture")
  assert.deepEqual(actions.at(-1), {
    action: "token_set_enabled",
    id: "tok_fixture",
    enabled: false,
    confirmed: true,
  })
  choices.push("Fast Mode…", "Allow")
  await run("manageToken", "tok_fixture")
  assert.deepEqual(actions.at(-1), { action: "token_fast_mode", id: "tok_fixture", enabled: true, confirmed: true })
  inputs.push("New token", "90")
  await run("createToken")
  assert.deepEqual(actions.at(-1), { action: "token_create", name: "New token", days: 90 })
  assert.ok((await rows("tokens")).every((r) => r.label !== "Manage profile tokens"))
  choices.push("First")
  await run("history")
  charts[0].message({ type: "chart", period: "7d" })
  await settle()
  assert.ok(args.some((a) => a.join(" ") === "quota-history 7 --period 7d --json"))
  assert.equal(charts[0].sent.at(-1)?.chart.series[0].points[0][1], 68)
  fail = true
  await run("refresh")
  assert.match(status.text, /stale/)
  assert.match(status.tooltip, /First.*\nSecond/)
  await settle()
  assert.match(usage.sent.at(-1)?.notice, /Stale report/)
  assert.ok((await rows("tokens")).some((r) => r.label === "Old label"))
  selectUsage("token")
  await settle()
  assert.equal(usage.sent.at(-1)?.chart, undefined)
  stamp++
  await run("refresh")
  assert.doesNotMatch(status.tooltip, /First|Second/)
  assert.ok(!(await rows("tokens")).some((r) => r.label === "Old label"))
  inputs.push("https://router.example/v1", "synthetic token")
  await run("connect")
  assert.equal(address, "https://router.example/v1")
  assert.equal(saved.get(secretKey(address)), "synthetic token")
})
