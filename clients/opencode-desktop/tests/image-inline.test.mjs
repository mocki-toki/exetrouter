import { test } from "node:test"
import assert from "node:assert/strict"
import { readFile } from "node:fs/promises"
import * as files from "../src/image-files.mjs"
import * as inline from "../src/image-inline.mjs"

const png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII="
const file = { type: "file", mime: "image/png", uri: png }
const token = "image-reference-001"
const fence = id => `\`\`\`fleet-image\n${id}\n\`\`\``
const text = value => ({ type: "text", text: value })
const tool = (id = "t1", status = "completed", name = "exetrouter_generate_image") => ({ type: "tool", id, name,
  time: { completed: 1 }, state: { status, input: { prompt: "PRIVATE PROMPT" }, content: [file, text("PRIVATE OUTPUT")] } })
const message = (id = "m1", status = "completed") => ({ type: "assistant", id, content: [text(fence(token)), tool("t1", status)] })
const page = (data, next) => ({ data, cursor: { next } })
const find = (data, id = token) => inline.findCard(data, id, files.collectImages)
const tick = () => new Promise(resolve => setImmediate(resolve))

test("saved raster images preserve safe filenames without prompts, tool text, duplicates or external fetches", () => {
  const edit = message("m2"); edit.content[1].name = "exetrouter_edit_image"
  const images = files.collectImages([message(), message(), edit])
  assert.equal(images.length, 2)
  assert.ok(images.every(image => image.uri === png && image.name.endsWith(".png")))
  assert.ok(!JSON.stringify(images).includes("PRIVATE"))
  for (const candidate of [
    { ...file, uri: "https://example.invalid/private" }, { ...file, uri: "file:///private.png" },
    { ...file, mime: "image/svg+xml", uri: "data:image/svg+xml;base64,PHN2Zy8+" },
    { ...file, uri: "data:image/png;base64,PHNjcmlwdD4=" }, { ...file, mime: "image/jpeg" },
    { ...file, uri: "data:image/png;base64,@@@@" }, { ...file, mime: "toString" },
  ]) assert.equal(files.imageFile(candidate), false)
  for (const status of ["running", "error"]) assert.deepEqual(files.collectImages([message("m1", status)]), [])
  assert.deepEqual(files.collectImages([{ ...message(), type: "user" }]), [])
})

test("marker parser accepts only complete dedicated fences, never nested examples, URLs or HTML", () => {
  assert.deepEqual(inline.markers(fence(token)), [token])
  assert.deepEqual(inline.markers(`~~~fleet-image\n${token}\n~~~`), [token])
  assert.deepEqual(inline.markers(`\`\`\`\`markdown\n${fence(token)}\n\`\`\`\``), [])
  assert.deepEqual(inline.markers(`\`\`\`fleet-image\n${token}`), [])
  for (const value of ["https://example.com/image", "<img src=x>", "short", "a".repeat(65), "a b c d e f"]) {
    assert.deepEqual(inline.markers(fence(value)), [])
  }
})

test("each marker binds to its following image call in the same assistant message, including parallel calls", () => {
  const second = "image-reference-002"
  const m = { type: "assistant", id: "m1", content: [text(`${fence(token)}\n${fence(second)}`),
    tool("other", "completed", "read"), tool("gen", "running"), tool("edit", "completed", "exetrouter_edit_image")] }
  assert.equal(find([m]).toolID, "gen")
  assert.equal(find([m]).status, "running")
  assert.equal(find([m], second).toolID, "edit")
  assert.equal(find([m], second).status, "complete")
  assert.ok(!JSON.stringify(find([m], second)).includes("PRIVATE"))
  assert.equal(find([{ ...message(), content: [tool(), text(fence(token))] }]).status, "waiting")
  assert.equal(find([{ type: "assistant", id: "a", content: [text(fence(token))] },
    { type: "assistant", id: "b", content: [tool()] }]).status, "waiting")
})

test("quoted tool output/user content cannot declare cards; duplicate references are rejected", () => {
  assert.equal(find([{ ...message(), type: "user" }]), undefined)
  assert.equal(find([{ ...message(), content: [{ ...tool(), state: { status: "completed", content: [text(fence(token))] } }] }]), undefined)
  assert.equal(find([message(), { ...message(), id: "duplicate" }]).status, "ambiguous")
  const m = message(); m.content.unshift(text(fence(token) + "\n"))
  assert.equal(find([m]).status, "ambiguous")
})

test("failed calls and missing/invalid image results are explicit, never presented as successful images", () => {
  assert.equal(find([message("m1", "error")]).status, "error")
  const m = message(); m.content[1].state.content = [text("done"), { ...file, uri: "https://example.invalid/x" }]
  assert.equal(find([m]).status, "unavailable")
  assert.deepEqual(find([m]).images, [])
})

test("pagination locates old cards once; subsequent lifecycle updates read only that exact message", async () => {
  const reads = [], exact = [], views = []
  const pages = [page([], "older"), page([message("old", "running")])]
  let latest = message("old")
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: async input => { reads.push(input); return pages.shift() },
    readMessage: async (id, signal) => { exact.push({ id, signal }); return latest }, onChange: state => views.push(state) })
  await card.refresh()
  assert.deepEqual(reads.map(input => input.cursor), [undefined, "older"])
  assert.equal(views.at(-1).status, "running")
  card.event("session.tool.success", { data: { assistantMessageID: "different", id: "t1" } })
  card.event("session.tool.success", { data: { assistantMessageID: "old", id: "different" } })
  await tick(); assert.equal(exact.length, 0)
  card.event("session.tool.success", { data: { assistantMessageID: "old", id: "t1" } })
  await tick()
  assert.equal(exact.length, 1)
  assert.equal(exact[0].id, "old")
  assert.equal(views.at(-1).status, "complete")
  assert.equal(views.at(-1).images[0].uri, png)
  card.event("session.execution.succeeded", { data: {} })
  await tick(); assert.equal(exact.length, 1, "finished historical images are not downloaded on later executions")
  card.dispose()
})

test("a marker without an executed image call stops waiting when its run ends", async () => {
  const views = []
  const onlyMarker = { type: "assistant", id: "m1", content: [text(fence(token))] }
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: async () => page([onlyMarker]), readMessage: async () => onlyMarker, onChange: state => views.push(state) })
  await card.refresh()
  assert.equal(views.at(-1).status, "waiting")
  card.event("session.execution.succeeded", { data: {} })
  await tick()
  assert.equal(views.at(-1).status, "unavailable")
  card.dispose()
})

test("a streaming marker not yet present in the message read is resolved on the following tool event", async () => {
  const views = []
  let reads = 0
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: async () => page(++reads === 1 ? [] : [message("m1", "running")]),
    readMessage: async () => message(), onChange: state => views.push(state) })
  await card.refresh()
  assert.equal(views.at(-1).status, "unavailable")
  card.event("session.tool.called", { data: { assistantMessageID: "m1", id: "t1" } })
  await tick()
  assert.equal(views.at(-1).status, "running")
  card.event("session.tool.success", { data: { assistantMessageID: "m1", id: "t1" } })
  await tick()
  assert.equal(views.at(-1).status, "complete")
  card.dispose()
})

test("lifecycle events arriving during a read are coalesced and a later completion is not lost", async () => {
  const views = [], reads = []
  let finish
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: () => new Promise(resolve => { finish = resolve }),
    readMessage: async id => { reads.push(id); return message() }, onChange: state => views.push(state) })
  const pending = card.refresh()
  card.event("session.tool.called", { data: { assistantMessageID: "m1", id: "t1" } })
  card.event("session.tool.success", { data: { assistantMessageID: "m1", id: "t1" } })
  finish(page([message("m1", "running")]))
  await pending
  assert.deepEqual(reads, ["m1"])
  assert.equal(views.at(-1).status, "complete")
  card.dispose()
})

test("repeated cursors terminate and failed reads disclose no private exception and do not automatically retry", async () => {
  let reads = 0
  const views = []
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: async () => { reads++; return page([], "same") }, onChange: state => views.push(state) })
  await card.refresh()
  assert.equal(reads, 2)
  assert.equal(views.at(-1).status, "unavailable")
  card.dispose()
  const failed = inline.createCard({ token, collectImages: files.collectImages,
    read: async () => { reads++; throw Error("PRIVATE") }, onChange: state => views.push(state) })
  await failed.refresh(); await tick()
  assert.equal(reads, 3)
  assert.equal(views.at(-1).status, "read-error")
  assert.ok(!JSON.stringify(views).includes("PRIVATE"))
  failed.dispose()
})

test("unmount/session switch aborts reads and refuses stale image responses", async () => {
  let finish, signal
  const views = []
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: input => { signal = input.signal; return new Promise(resolve => { finish = resolve }) }, onChange: state => views.push(state) })
  const pending = card.refresh()
  card.dispose()
  finish(page([message()]))
  await pending
  assert.equal(signal.aborted, true)
  assert.equal(views.length, 0)
})

test("interruption stops animation even if a stale running snapshot arrives; no generation is retried", async () => {
  const views = []
  let reads = 0
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: async () => { reads++; return page([message("m1", "running")]) },
    readMessage: async () => { reads++; return message("m1", "running") }, onChange: state => views.push(state) })
  await card.refresh()
  card.event("session.execution.interrupted", { data: {} })
  assert.equal(views.at(-1).status, "interrupted")
  await card.refresh()
  assert.equal(views.at(-1).status, "interrupted")
  assert.equal(reads, 2)
  card.dispose()
})

test("session deletion aborts work and removes retained images", async () => {
  const views = []
  const card = inline.createCard({ token, collectImages: files.collectImages,
    read: async () => page([message()]), onChange: state => views.push(state) })
  await card.refresh()
  assert.equal(views.at(-1).images.length, 1)
  card.event("session.deleted", { data: {} })
  assert.equal(views.at(-1).images.length, 0)
  await card.refresh()
  assert.equal(views.length, 2)
})

class Element {
  children = []; style = {}; attrs = {}
  append(...nodes) { this.children.push(...nodes) }
  replaceChildren(...nodes) { this.children = nodes }
  setAttribute(key, value) { this.attrs[key] = value }
  getAttribute(key) { return this.attrs[key] }
}
function dom() {
  const nodes = []
  return { nodes, document: { createElement: tag => { const node = new Element(); node.tag = tag; nodes.push(node); return node } } }
}

test("inline card preserves running timelines, opens fullscreen and offers non-generating read retry", () => {
  const { nodes, document } = dom(), root = new Element()
  let reads = 0
  inline.renderCard(root, { status: "running", images: [] }, () => reads++, document)
  assert.equal(root.attrs["aria-busy"], "true")
  assert.ok(nodes.some(node => node.attrs.role === "status"))
  const waitingBox = root.children[0], count = nodes.length
  inline.renderCard(root, { status: "running", images: [] }, () => reads++, document)
  assert.equal(root.children[0], waitingBox)
  assert.equal(nodes.length, count)
  inline.renderCard(root, find([message()]), () => reads++, document)
  assert.equal(root.attrs["aria-busy"], "false")
  const img = nodes.find(node => node.tag === "img")
  assert.equal(img.src, png)
  const download = nodes.find(node => node.tag === "a")
  assert.equal(download.href, png)
  assert.ok(download.download.endsWith(".png"))
  const zoom = nodes.find(node => node.attrs["aria-haspopup"] === "dialog")
  assert.equal(typeof zoom.onclick, "function")
  img.onerror(); assert.ok(nodes.some(node => node.textContent?.includes("download original")))
  inline.renderCard(root, { status: "read-error" }, () => reads++, document)
  nodes.find(node => node.textContent === "Read result again").onclick()
  assert.equal(reads, 1)
  assert.match(inline.css, /prefers-reduced-motion:reduce/)
  assert.doesNotMatch(inline.css, /https?:/)
})

test("public renderer reads its owning session/server, not the selected chat, and never registers a panel", async t => {
  const { nodes, document } = dom()
  const previous = globalThis.document
  globalThis.document = document
  t.after(() => { globalThis.document = previous })
  const registry = [], calls = [], listeners = new Map(), cleanups = []
  const session = { key: "server-a\ns1", id: "s1", server: { client: {
    message: { list: async (input, options) => { calls.push({ input, options }); return page([message("m1", "running")]) } },
    session: { message: { get: async input => { calls.push(input); return message() } } },
  }, data: { on: (type, fn) => { listeners.set(type, fn); return () => listeners.delete(type) } } } }
  const ctx = { add: (kind, value) => registry.push({ kind, value }) }
  inline.installInlineImages(ctx, { CodeBlock: "code", Style: "style", collectImages: files.collectImages,
    createKeyed: (source, run) => { const value = source(); if (value) run(value) }, onCleanup: fn => cleanups.push(fn) })
  assert.deepEqual(registry.map(entry => entry.kind), ["style", "code"])
  const renderer = registry[1].value
  assert.equal(renderer.render({ code: "https://example.com", session }), undefined)
  assert.equal(renderer.render({ code: token }), undefined)
  const root = renderer.render({ code: token, session })
  await tick()
  assert.deepEqual(calls[0].input, { sessionID: "s1", type: "assistant", limit: 20, order: "desc" })
  assert.equal(calls.length, 1)
  assert.equal(root.attrs["aria-busy"], "true")
  listeners.get("session.tool.success")({ data: { sessionID: "other", assistantMessageID: "m1", id: "t1" } })
  await tick(); assert.equal(calls.length, 1)
  listeners.get("session.tool.success")({ data: { sessionID: "s1", assistantMessageID: "m1", id: "t1" } })
  await tick()
  assert.deepEqual(calls[1], { sessionID: "s1", messageID: "m1" })
  assert.ok(nodes.some(node => node.tag === "img"))
  for (const cleanup of cleanups.reverse()) cleanup()
  assert.equal(listeners.size, 0)
  assert.equal(root.children.length, 0)
})

test("presentation instructions apply only when actual image tools are available; old gallery is absent", async () => {
  assert.equal(inline.enabled({ exetrouter_generate_image: {} }), true)
  assert.equal(inline.enabled({ exetrouter_edit_image: {} }), true)
  assert.equal(inline.enabled({ shell: {} }), false)
  const source = await readFile(new URL("../src/window.tsx", import.meta.url), "utf8")
  assert.doesNotMatch(source, /installGallery|slash: \{ name: "images"|FLEET_IMAGE_GALLERY/)
  assert.match(inline.instructions, /never retry an interrupted or uncertain image call automatically/)
  assert.match(inline.instructions, /before EACH/)
})
