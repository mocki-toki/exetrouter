import assert from "node:assert/strict"
import { createServer } from "node:http"
import { test } from "node:test"
import { Schema } from "effect"
import { Catalog, loadModels, parseModels, type Auth } from "../src/catalog"
import { options, apiURL } from "../src/options"
import { CatalogStatus } from "../src/schemas"
import { catalog, deferred } from "./fixtures"

const config = options({ baseURL: "https://api.example.com/v1" })
const auth = (key: string): Auth => ({ connection: { type: "env", name: "EXETROUTER_TOKEN" }, token: key })

test("validates endpoints and refuses secrets and ambiguous plugin options", () => {
  assert.equal(apiURL("http://[::1]:8787/v1/"), "http://[::1]:8787/v1")
  for (const url of ["https://user:secret@example.com/v1", "http://api.example.com/v1", "file:///v1", "https://api.example.com/v1?token=synthetic", "https://api.example.com/v1#x", "https://api.example.com/v2", "https://api.example.com\\v1"]) {
    assert.throws(() => apiURL(url), /endpoint/)
  }
  assert.throws(() => options({ token: "synthetic-secret" }), /credential|connection/)
  assert.throws(() => options({ refreshSeconds: 1 }))
  assert.throws(() => options({ exrBinary: "exr\ncommand" }))
})

test("projects only router model metadata, without catalog inheritance or executable overlays", () => {
  const value = catalog()
  Object.assign(value.providers.exetrouter.models["test-reasoner"], {
    package: "untrusted-package", headers: { authorization: "synthetic-secret" }, body: { input: "untrusted-prompt" },
  })
  Object.assign(value.providers.exetrouter.models["test-reasoner"].settings, { baseURL: "https://other.example.com/v1", apiKey: "synthetic-secret" })
  const [model] = parseModels(value)
  assert.equal(model.id, "test-reasoner")
  assert.equal(model.providerID, "exetrouter")
  assert.equal(model.package, undefined)
  assert.equal(model.canonical, undefined)
  assert.equal(model.limit.output, 0)
  assert.deepEqual(model.variants.map((v) => v.id), ["low", "high", "future"])
  assert.deepEqual(model.capabilities.input, ["text", "image"])
  assert.equal(model.settings?.reasoningEffort, "low")
  assert.doesNotMatch(JSON.stringify(model), /untrusted|synthetic-secret|baseURL/)
  assert.deepEqual(parseModels({ providers: { exetrouter: { models: {} } } }), [])
})

test("rejects incomplete, invalid and conflicting model inventories atomically", () => {
  const value = catalog()
  value.providers.exetrouter.models["test-reasoner"].variants.push({ id: "high", settings: { reasoningEffort: "high" } })
  assert.throws(() => parseModels(value))
  assert.throws(() => parseModels({ providers: { exetrouter: { models: { bad: {} } } } }))
  const negative = catalog()
  negative.providers.exetrouter.models["test-reasoner"].limit.input = -1
  assert.throws(() => parseModels(negative))
})

test("reads only the authenticated catalog route and never exposes error bodies", async () => {
  const signal = new AbortController().signal
  let calls = 0
  const request: typeof fetch = async (url, init) => {
    calls++
    assert.equal(url, "https://api.example.com/v1/models/opencode-v2")
    assert.equal(init?.redirect, "error")
    assert.deepEqual(init?.headers, { authorization: "Bearer synthetic-token" })
    return Response.json(catalog())
  }
  assert.equal((await loadModels(config.baseURL, "synthetic-token", signal, request)).length, 1)
  assert.equal(calls, 1)
  await assert.rejects(loadModels(config.baseURL, "synthetic-token", signal, async () => new Response("private-body-marker", { status: 401 })), (e: Error) => !e.message.includes("private-body-marker"))
  await assert.rejects(loadModels(config.baseURL, "synthetic-token", signal, async () => new Response("x".repeat(2 * 1024 * 1024 + 1))), /could not be read/)
})

test("native fetch does not follow a catalog redirect or forward the bearer", async (t) => {
  let redirected = 0
  const server = createServer((req, res) => {
    if (req.url === "/v1/models/opencode-v2") { res.writeHead(302, { location: "/capture" }); res.end() }
    else { redirected++; res.end("unexpected") }
  })
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve))
  t.after(() => { server.closeAllConnections(); server.close() })
  const address = server.address()
  assert.ok(address && typeof address !== "string")
  await assert.rejects(loadModels(`http://127.0.0.1:${address.port}/v1`, "synthetic-token", AbortSignal.timeout(5000)))
  assert.equal(redirected, 0)
})

test("retains same-connection inventory on a temporary failure, but drops it on sign-out", async () => {
  let current: Auth | undefined = auth("first")
  let fail = false
  const source = new Catalog(config, async () => current, async () => {}, async () => {
    if (fail) throw new Error("private error body")
    return parseModels(catalog())
  })
  assert.equal((await source.refresh()).state, "ready")
  const fetched = source.status.fetchedAt
  fail = true
  assert.equal((await source.refresh()).state, "stale")
  assert.equal(source.status.models, 1)
  assert.equal(source.status.fetchedAt, fetched)
  current = undefined
  assert.equal((await source.refresh()).state, "needs_token")
  assert.equal(source.inventory, undefined)
  assert.equal(source.status.fetchedAt, null)
  Schema.decodeUnknownSync(CatalogStatus)(source.status)
  await source.close()
})

test("restores only a recent catalog for the exact endpoint and credential without fetching", async () => {
  const original = auth("reload-fixture")
  const source = new Catalog(config, async () => original, async () => {}, async () => parseModels(catalog()))
  const ready = await source.refresh()
  await source.close()
  const noNetwork = async () => { throw new Error("restore must not fetch") }
  let publications = 0
  const restored = new Catalog(config, async () => original, async () => { publications++ }, noNetwork)
  await restored.restore()
  assert.equal(publications, 1)
  assert.deepEqual(restored.status, { ...ready, state: "stale" })
  assert.equal(restored.inventory?.models[0].id, "test-reasoner")
  await restored.close()

  for (const [endpoint, credential] of [
    [config, auth("replacement")],
    [config, { ...original, connection: { type: "env", name: "OTHER_TOKEN" } } satisfies Auth],
    [{ ...config, baseURL: "https://other.example.com/v1" }, original],
    [config, undefined],
  ] as const) {
    const isolated = new Catalog(endpoint, async () => credential, async () => assert.fail("published an unrelated catalog"), noNetwork)
    await isolated.restore()
    assert.equal(isolated.inventory, undefined)
    await isolated.close()
  }
})

test("unchanged refreshes do not invalidate the OpenCode model registry", async () => {
  let publications = 0
  let input = 80000
  const source = new Catalog(config, async () => auth("steady-fixture"), async () => { publications++ }, async () => {
    const value = catalog()
    value.providers.exetrouter.models["test-reasoner"].limit.input = input
    return parseModels(value)
  })
  await source.refresh()
  await source.refresh()
  assert.equal(publications, 1)
  input = 60000
  await source.refresh()
  assert.equal(publications, 2)
  await source.close()
})

test("an in-flight old token cannot publish inventory after replacement, even with the same connection id", async () => {
  let current = auth("first")
  const first = deferred<ReturnType<typeof parseModels>>()
  const started = deferred<void>()
  const calls: string[] = []
  const source = new Catalog(config, async () => current, async () => {}, async (_, token) => {
    calls.push(token)
    if (token === "first") { started.resolve(); return first.promise }
    return []
  })
  const pending = source.refresh()
  await started.promise
  current = auth("second")
  first.resolve(parseModels(catalog()))
  await pending
  assert.deepEqual(calls, ["first", "second"])
  assert.equal(source.status.state, "ready")
  assert.equal(source.status.models, 0)
  assert.deepEqual(source.inventory?.models, [])
  await source.close()
})

test("coalesces concurrent refreshes and aborts work on unload", async () => {
  let active = 0
  let maximum = 0
  const started = deferred<void>()
  const source = new Catalog(config, async () => auth("first"), async () => {}, async (_, __, signal) => {
    maximum = Math.max(maximum, ++active)
    started.resolve()
    await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }))
    active--
    return []
  })
  const first = source.refresh()
  const second = source.refresh()
  assert.equal(first, second)
  await started.promise
  await source.close()
  assert.equal(maximum, 1)
  assert.equal(source.inventory, undefined)
})

test("credential resolution failure clears cached models and sign-out returns needs_token", async () => {
  let unavailable = false
  let signedOut = false
  const source = new Catalog(config, async () => {
    if (unavailable) throw new Error("Private credential resolution detail")
    return signedOut ? undefined : auth("first")
  }, async () => {}, async () => parseModels(catalog()))
  await source.refresh()
  unavailable = true
  assert.equal((await source.refresh()).state, "error")
  assert.equal(source.inventory, undefined)
  unavailable = false
  signedOut = true
  assert.equal((await source.refresh()).state, "needs_token")
  await source.close()
})
