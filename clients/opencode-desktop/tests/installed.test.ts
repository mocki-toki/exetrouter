import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { once } from "node:events"
import { mkdtemp, mkdir, writeFile, rm, readFile } from "node:fs/promises"
import { createServer } from "node:http"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { test } from "node:test"
import { setTimeout as delay } from "node:timers/promises"
import { OpenCode } from "@opencode/client"
import { ExetRouter } from "../src/rpc"
import { catalog } from "./fixtures"

const binary = process.env.EXETROUTER_OPENCODE_PLUGIN_BIN

test("packed plugin loads and recovers legacy history on stock OpenCode against a loopback router", { skip: !binary, timeout: 120_000 }, async (t) => {
  const version = execFileSync(binary!, ["--version"], { encoding: "utf8" })
  assert.match(version, /\bv2\.(?:2\.0|4\.2)\b/)
  const testReplay = version.includes("v2.4.2")
  const root = await mkdtemp(join(tmpdir(), "exetrouter-opencode-plugin-"))
  let stop: (() => Promise<void>) | undefined
  let closeRouter: (() => void) | undefined
  t.after(async () => { await stop?.(); closeRouter?.(); await rm(root, { recursive: true, force: true }) })
  const source = resolve(dirname(fileURLToPath(import.meta.url)), "..")
  const packed = JSON.parse(execFileSync("npm", ["pack", "--json", "--ignore-scripts", "--pack-destination", root], { cwd: source, encoding: "utf8" })) as { filename: string }[]
  const install = join(root, "install")
  await mkdir(install)
  // Seed the exact dependency tree instead of resolving newer transitive dependencies offline.
  const manifest = JSON.parse(await readFile(join(source, "package.json"), "utf8"))
  const lock = JSON.parse(await readFile(join(source, "package-lock.json"), "utf8"))
  manifest.name = lock.name = "exetrouter-plugin-fixture"
  lock.packages[""].name = manifest.name
  await writeFile(join(install, "package.json"), JSON.stringify(manifest))
  await writeFile(join(install, "package-lock.json"), JSON.stringify(lock))
  execFileSync("npm", ["ci", "--offline", "--ignore-scripts", "--no-audit", "--no-fund", "--prefix", install], { encoding: "utf8", timeout: 60_000 })
  execFileSync("npm", ["install", "--offline", "--ignore-scripts", "--no-audit", "--no-fund", "--prefix", install, join(root, packed[0].filename)], { encoding: "utf8", timeout: 60_000 })
  const installed = join(install, "node_modules", "opencode-exetrouter")
  assert.ok((await readFile(join(installed, "dist/gui-experimental.cjs"))).length > 0)

  let generationRequests = 0
  let enableReplay = false
  const requests: Array<{ input: unknown, model: string, prompt_cache_key: string, ticket: string | undefined }> = []
  const exchanges: Array<{ chat: string, model: string, checkpoint: string }> = []
  const syntheticProof = `exrctx1.${"x".repeat(86)}.synthetic-checkpoint`
  const syntheticTicket = `exrctx1.${"y".repeat(86)}.synthetic-ticket`
  const tokens: string[] = []
  const router = createServer(async (req, res) => {
    if (enableReplay && req.method === "POST" && (req.url === "/v1/responses/replay" || req.url === "/v1/responses")) {
      let buffer = ""
      for await (const chunk of req) buffer += chunk.toString()
      const body = JSON.parse(buffer)
      if (req.url.endsWith("/replay")) {
        exchanges.push(body)
        res.writeHead(204, { "x-exetrouter-replay-ticket": syntheticTicket }).end()
        return
      }
      requests.push({ input: body.input, model: body.model, prompt_cache_key: body.prompt_cache_key, ticket: req.headers["x-exetrouter-replay-ticket"]?.toString() })
      const id = `resp_${requests.length}`
      const text = "## Objective\n- Preserve saved context"
      const item = { id: `msg_${requests.length}`, type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text, annotations: [] }] }
      const common = { id, model: body.model, created_at: 1_700_000_000, object: "response" }
      res.writeHead(200, { "content-type": "text/event-stream" }).end([
        { type: "response.created", response: { ...common, status: "in_progress", output: [] } },
        { type: "response.output_item.added", output_index: 0, item: { ...item, status: "in_progress", content: [] } },
        { type: "response.output_text.delta", output_index: 0, content_index: 0, item_id: item.id, delta: text },
        { type: "response.output_item.done", output_index: 0, item },
        { type: "response.completed", response: { ...common, status: "completed", output: [item], usage: { input_tokens: 10, output_tokens: 5, total_tokens: 15 } } },
      ].map((event) => `data: ${JSON.stringify(event)}\n\n`).join(""))
      return
    }
    if (req.url !== "/v1/models/opencode-v2" || req.method !== "GET") {
      generationRequests++
      res.writeHead(400).end()
      return
    }
    tokens.push(req.headers.authorization ?? "")
    const value = catalog()
    if (req.headers.authorization === "Bearer synthetic-second") value.providers.exetrouter.models["test-reasoner"].limit.input = 70000
    res.setHeader("content-type", "application/json")
    res.end(JSON.stringify(value))
  })
  await new Promise<void>((resolve) => router.listen(0, "127.0.0.1", resolve))
  closeRouter = () => { router.closeAllConnections(); router.close() }
  const address = router.address()
  assert.ok(address && typeof address !== "string")
  const home = join(root, "home")
  const work = join(root, "project")
  await mkdir(home)
  await mkdir(work)
  const start = async () => {
    const child = spawn(binary!, ["serve", "--hostname", "127.0.0.1", "--port", "0", "--print-logs", "--log-level", "debug"], {
      cwd: work,
      stdio: ["ignore", "pipe", "pipe"],
      env: {
        PATH: process.env.PATH, TMPDIR: process.env.TMPDIR,
        HOME: home, XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"),
        XDG_STATE_HOME: join(root, "state"), XDG_CACHE_HOME: join(root, "cache"),
        OPENCODE_CONFIG_DIR: join(root, "config"),
        OPENCODE_CONFIG_CONTENT: JSON.stringify({
          plugins: [{ package: installed, options: { baseURL: `http://127.0.0.1:${address.port}/v1`, transport: testReplay ? "http" : "websocket" } }],
        }),
        OPENCODE_PASSWORD: "synthetic-server-password",
        OPENCODE_DISABLE_MODELS_FETCH: "true", OPENCODE_DISABLE_AUTOUPDATE: "true",
        OPENCODE_DISABLE_PROJECT_CONFIG: "true", OPENCODE_DISABLE_CLAUDE_CODE: "true",
        OPENCODE_DISABLE_FILEWATCHER: "true", OPENCODE_DISABLE_FFF: "true",
        EXETROUTER_TOKEN: "synthetic-first",
      },
    })
    let diagnostics = ""
    child.stderr.on("data", (data) => { diagnostics = (diagnostics + data.toString()).slice(-40_000) })
    stop = async () => {
      if (child.exitCode !== null || child.signalCode !== null) return
      const exit = once(child, "exit")
      child.kill("SIGTERM")
      const timer = setTimeout(() => child.kill("SIGKILL"), 3000)
      try { await exit } finally { clearTimeout(timer) }
    }
    const url = await new Promise<string>((resolve, reject) => {
      let output = ""
      const timer = setTimeout(() => reject(new Error("Isolated OpenCode startup timed out.")), 30_000)
      child.once("error", () => { clearTimeout(timer); reject(new Error("Cannot start isolated OpenCode.")) })
      child.once("exit", () => { clearTimeout(timer); reject(new Error("Isolated OpenCode exited before listening.")) })
      child.stdout.on("data", (chunk: Buffer) => {
        output = (output + chunk.toString("utf8")).slice(-16384)
        const match = output.match(/server listening on (http:\/\/127\.0\.0\.1:\d+)/)
        if (match) { clearTimeout(timer); resolve(match[1]) }
      })
    })
    return { url, logs: () => diagnostics, client: OpenCode.make({ baseUrl: url, headers: { authorization: `Basic ${Buffer.from("opencode:synthetic-server-password").toString("base64")}` } }) }
  }
  let server = await start()
  let client = server.client
  const request = { signal: AbortSignal.timeout(30_000) }
  const location = { directory: work }
  // Locations activate plugins asynchronously; model reads wait for that activation.
  await client.model.list({ location }, request)
  const findPlugin = async () => (await client.plugin.list({ location }, request)).data.find((item) => item.id === "exetrouter")
  let plugin = await findPlugin()
  for (let attempt = 0; !plugin && attempt < 100; attempt++) {
    await delay(50)
    plugin = await findPlugin()
  }
  if (!plugin) {
    assert.fail(`Packaged plugin is missing: ${server.logs().split("\n").filter((line) => /plugin|config/i.test(line)).slice(-20).join("\n")}`)
  }
  assert.equal(plugin.state.status, "active")
  assert.equal(plugin.features.gui, true)
  const gui = (await client.plugin.gui({ id: "exetrouter", location }, request)).data
  assert.match(gui.code, /ExetRouter/)
  assert.equal(gui.options.baseURL, `http://127.0.0.1:${address.port}/v1`)
  assert.equal((await client.rpc(ExetRouter).refresh({}, { ...request, location })).state, "ready")
  let models = (await client.model.list({ location }, request)).data.filter((model) => model.providerID === "exetrouter")
  assert.equal(models.length, 1)
  assert.equal(models[0].id, "test-reasoner")
  assert.equal(models[0].limit.input, 80000)
  assert.equal(models[0].limit.output, 0)
  assert.deepEqual(models[0].variants.map((variant) => variant.id), ["low", "high", "future"])
  assert.deepEqual(models[0].capabilities.input, ["text", "image"])
  const provider = (await client.provider.get({ providerID: "exetrouter", location }, request)).data
  assert.equal(provider.package, "@opencode/ai/providers/openai/responses")
  assert.equal(provider.settings?.transport, testReplay ? "http" : "websocket")
  assert.equal(provider.settings?.store, false)
  assert.deepEqual(provider.settings?.compaction, { type: "summary" })

  await client.integration.connect.key({ integrationID: "exetrouter", key: "synthetic-second", location }, request)
  await client.rpc(ExetRouter).refresh({}, { ...request, location })
  models = (await client.model.list({ location }, request)).data.filter((model) => model.providerID === "exetrouter")
  assert.equal(models[0].limit.input, 70000)
  assert.ok(tokens.includes("Bearer synthetic-first"))
  assert.ok(tokens.includes("Bearer synthetic-second"))
  assert.equal(generationRequests, 0)

  if (!testReplay) return
  enableReplay = true
  const selected = { providerID: "exetrouter", id: "test-reasoner", variant: "high" }
  const now = Date.now()
  const time = { created: now }
  const template = await client.session.create({ location, title: "Template" })
  assert.ok(template)
  const sessionID = "ses_synthetic_legacy"
  const post = async (path: string, body: unknown) => {
    const response = await fetch(server.url + path, {
      method: "POST", signal: AbortSignal.timeout(15_000),
      headers: { authorization: `Basic ${Buffer.from("opencode:synthetic-server-password").toString("base64")}`, "content-type": "application/json" },
      body: JSON.stringify(body),
    })
    assert.ok(response.ok, `${path} returned ${response.status}: ${await response.text()}`)
  }
  await post("/api/experimental/session/import", {
    location, info: { ...template, id: sessionID, title: "Legacy test", model: selected },
    messages: [
      { id: "msg_saved_user", type: "user", time, text: "Original user text" },
      { id: "msg_saved_assistant", type: "assistant", agent: "build", model: selected, time: { ...time, completed: now }, finish: "stop", content: [
        { type: "text", text: "Saved assistant text" },
        { type: "tool", id: "call_saved", name: "echo", time: { ...time, completed: now }, state: {
          status: "completed", input: { text: "Saved tool input" }, content: [{ type: "text", text: "Saved tool result" }],
        } },
      ] },
      { id: "msg_saved_checkpoint", type: "compaction", status: "completed", reason: "manual", time, model: selected,
        summary: "", native: { messages: [{ role: "assistant", content: [{ type: "compaction", provider: "exetrouter", encrypted: syntheticProof }] }] } },
    ],
  })
  const wait = () => client.experimental.session.wait({ sessionID }, { signal: AbortSignal.timeout(15_000) })
  await client.session.prompt({ sessionID, text: "First post-checkpoint question" })
  await wait()
  assert.equal(exchanges.length, 1)
  assert.equal(exchanges[0].checkpoint, syntheticProof)
  assert.equal(exchanges[0].model, selected.id)
  assert.equal(requests[0]?.model, selected.id)
  assert.match(JSON.stringify(requests[0]?.input), /Original user text/)
  assert.match(JSON.stringify(requests[0]?.input), /Saved assistant text/)
  assert.match(JSON.stringify(requests[0]?.input), /Saved tool result/)
  assert.doesNotMatch(JSON.stringify(requests[0]?.input), /synthetic-checkpoint/)
  await client.session.compact({ sessionID })
  await wait()
  await stop?.()
  server = await start()
  client = server.client
  const restartRequest = { signal: AbortSignal.timeout(15_000), location }
  await client.model.list({ location }, restartRequest)
  let routerReady = false
  for (let attempt = 0; !routerReady && attempt < 100; attempt++) {
    routerReady = await client.rpc(ExetRouter).refresh({}, restartRequest).then((result) => result.state === "ready", () => false)
    if (!routerReady) await delay(50)
  }
  assert.ok(routerReady, "the restarted service loads the same provider")
  await client.session.prompt({ sessionID, text: "Question after portable summary" })
  await wait()
  assert.equal(exchanges.length, 1)
  assert.equal(requests.length, 3)
  assert.ok(requests[0].prompt_cache_key)
  assert.deepEqual(requests.map(({ model, prompt_cache_key, ticket }) => ({ model, prompt_cache_key, ticket })),
    Array(3).fill({ model: selected.id, prompt_cache_key: requests[0].prompt_cache_key, ticket: syntheticTicket }))
  assert.match(JSON.stringify(requests[2].input), /<summary>.*Preserve saved context/)
  assert.deepEqual((await client.session.get({ sessionID })).model, selected)
  await client.session.switchModel({ sessionID: template.id, model: selected })
  await client.session.prompt({ sessionID: template.id, text: "Ordinary chat" })
  await client.experimental.session.wait({ sessionID: template.id }, { signal: AbortSignal.timeout(15_000) })
  assert.equal(exchanges.length, 1)
  assert.equal(requests[3]?.ticket, undefined)
  assert.match(JSON.stringify(requests[3]?.input), /Ordinary chat/)
})
