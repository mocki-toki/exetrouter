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

test("packed plugin loads in supported OpenCode 2.2–2.4 with native credentials and a loopback catalog, without inference", { skip: !binary, timeout: 120_000 }, async (t) => {
  assert.match(execFileSync(binary!, ["--version"], { encoding: "utf8" }), /\bv?2\.[234]\.\d+\b/)
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
  execFileSync("npm", ["ci", "--offline", "--ignore-scripts", "--no-audit", "--no-fund"], { cwd: install, encoding: "utf8", timeout: 60_000 })
  execFileSync("npm", ["install", "--offline", "--ignore-scripts", "--no-audit", "--no-fund", join(root, packed[0].filename)], { cwd: install, encoding: "utf8", timeout: 60_000 })
  const installed = join(install, "node_modules", "opencode-exetrouter")
  assert.ok((await readFile(join(installed, "dist/gui-experimental.cjs"))).length > 0)

  let generationRequests = 0
  const tokens: string[] = []
  const router = createServer((req, res) => {
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
  const config = join(root, "config", "opencode")
  await mkdir(config, { recursive: true })
  await writeFile(join(config, "opencode.json"), JSON.stringify({
    plugins: [{ package: installed, options: { baseURL: `http://127.0.0.1:${address.port}/v1` } }],
  }))
  const child = spawn(binary!, ["serve", "--hostname", "127.0.0.1", "--port", "0"], {
    cwd: work,
    stdio: ["ignore", "pipe", "ignore"],
    env: {
      PATH: process.env.PATH, TMPDIR: process.env.TMPDIR,
      HOME: home, XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"),
      XDG_STATE_HOME: join(root, "state"), XDG_CACHE_HOME: join(root, "cache"),
      OPENCODE_PASSWORD: "synthetic-server-password",
      OPENCODE_DISABLE_MODELS_FETCH: "true", OPENCODE_DISABLE_AUTOUPDATE: "true",
      OPENCODE_DISABLE_PROJECT_CONFIG: "true", OPENCODE_DISABLE_CLAUDE_CODE: "true",
      OPENCODE_DISABLE_FILEWATCHER: "true", OPENCODE_DISABLE_FFF: "true",
      EXETROUTER_TOKEN: "synthetic-first",
    },
  })
  stop = async () => {
    const exit = once(child, "exit")
    child.kill("SIGTERM")
    const timer = setTimeout(() => child.kill("SIGKILL"), 3000)
    try { if (child.exitCode === null && child.signalCode === null) await exit }
    finally { clearTimeout(timer) }
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
  const client = OpenCode.make({ baseUrl: url, headers: { authorization: `Basic ${Buffer.from("opencode:synthetic-server-password").toString("base64")}` } })
  const request = { signal: AbortSignal.timeout(30_000) }
  const location = { directory: work }
  // Plugin registration is asynchronous on recent hosts even after the first model read.
  await client.model.list({ location }, request)
  let plugins = await client.plugin.list({ location }, request)
  const findPlugin = () => plugins.data.find((item) => item.id === "exetrouter")
  for (let attempt = 0; !findPlugin() && attempt < 40; attempt++) {
    await delay(125)
    plugins = await client.plugin.list({ location }, request)
  }
  const plugin = findPlugin()
  assert.ok(plugin, `Packaged plugin is registered: ${JSON.stringify(plugins.data.filter((item) => item.source.type !== "builtin").map((item) => ({ id: item.id, state: item.state })))}`)
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
  assert.equal(provider.settings?.transport, "websocket")
  assert.equal(provider.settings?.store, false)
  assert.deepEqual(provider.settings?.compaction, { type: "native" })

  await client.integration.connect.key({ integrationID: "exetrouter", key: "synthetic-second", location }, request)
  await client.rpc(ExetRouter).refresh({}, { ...request, location })
  models = (await client.model.list({ location }, request)).data.filter((model) => model.providerID === "exetrouter")
  assert.equal(models[0].limit.input, 70000)
  assert.ok(tokens.includes("Bearer synthetic-first"))
  assert.ok(tokens.includes("Bearer synthetic-second"))
  assert.equal(generationRequests, 0)
})
