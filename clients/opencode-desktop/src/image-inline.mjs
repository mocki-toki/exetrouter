// Public CodeBlock UI only: resolve an assistant's marker to its following image
// call in that same message. Never mutate history, replay tools or fetch image URLs.
import { animateParticles, openImageViewer, imageCss } from "./image-visuals.mjs"
const imageTools = new Set(["exetrouter_generate_image", "exetrouter_edit_image"])
const tokenPattern = /^[a-zA-Z0-9][a-zA-Z0-9_-]{7,63}$/
const instructions = `Image presentation in OpenCode Desktop:
Immediately before EACH ExetRouter image generation or editing tool call, emit a complete fenced code block with language fleet-image. Its only content must be a new unique 12–64 character ASCII identifier (letters, digits, hyphen or underscore). Never reuse an identifier from earlier in this session. Emit markers in the same order as their image calls, one marker per call. This is a UI reference, not a URL, path, prompt or image data.
The Desktop plugin replaces that marker with the real call's waiting state and saved result. Do not open a preview or gallery tab, repeat the image as base64/Markdown, or emit a second marker after completion. You may describe the result normally. Only generate/edit when the user requests it; never retry an interrupted or uncertain image call automatically.`

function markers(text) {
  const found = []
  let fence, lines = []
  for (const line of String(text ?? "").split(/\r?\n/)) {
    if (!fence) {
      const open = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line)
      if (open) { fence = { char: open[1][0], length: open[1].length, language: open[2].trim().split(/\s/)[0].toLowerCase() }; lines = [] }
    } else if (new RegExp(`^ {0,3}${fence.char}{${fence.length},}\\s*$`).test(line)) {
      const token = lines.join("\n").trim()
      if (fence.language === "fleet-image" && tokenPattern.test(token)) found.push(token)
      fence = undefined
    } else lines.push(line)
  }
  return found
}

function findCard(messages, token, collectImages) {
  const matches = []
  for (const message of messages ?? []) {
    if (message.type !== "assistant") continue
    const pending = []
    let text = ""
    const flush = () => {
      for (const id of markers(text)) {
        const card = { token: id, messageID: message.id, status: "waiting", images: [] }
        pending.push(card)
        if (id === token) matches.push(card)
      }
      text = ""
    }
    for (const part of message.content ?? []) {
      if (part.type === "text") { text += part.text ?? ""; continue }
      flush()
      if (part.type !== "tool" || !imageTools.has(part.name)) continue
      const card = pending.shift()
      if (!card) continue
      card.toolID = part.id
      const status = part.state?.status
      card.status = status === "completed" ? "complete" : status === "running" ? "running" :
        status === "error" ? "error" : "waiting"
      if (card.status === "complete") {
        card.images = collectImages([{ ...message, content: [part] }])
          .sort((a, b) => Number(a.id.split(":").at(-1)) - Number(b.id.split(":").at(-1)))
        if (!card.images.length) card.status = "unavailable"
      }
    }
    flush()
  }
  return matches.length > 1 ? { status: "ambiguous", images: [] } : matches[0]
}

function createCard({ token, read, readMessage, collectImages, onChange }) {
  let disposed = false, request, running = false, again = false, stopped = false, finished = false
  let state = { status: "loading", images: [] }
  const publish = next => {
    if (disposed) return
    state = stopped && ["waiting", "running"].includes(next.status) ? { ...next, status: "interrupted" } :
      finished && next.status === "waiting" && !next.toolID ? { ...next, status: "unavailable" } : next
    onChange(state)
  }
  async function refresh() {
    if (disposed) return
    if (running) { again = true; return }
    running = true
    request = new AbortController()
    try {
      do {
        again = false
        let card
        if (state.messageID) {
          const message = await readMessage(state.messageID, request.signal)
          card = findCard([message], token, collectImages)
        } else {
          let cursor
          const seen = new Set()
          do {
            const page = await read({ cursor, signal: request.signal })
            if (disposed) return
            if (!Array.isArray(page?.data)) throw Error("Invalid message page")
            card = findCard(page.data, token, collectImages)
            if (card) break
            cursor = page.cursor?.next
            if (seen.has(cursor)) break
            seen.add(cursor)
          } while (cursor)
        }
        if (disposed) return
        publish(card ?? { status: "unavailable", images: [] })
      } while (again && !disposed)
    } catch {
      // Reads can be retried explicitly. An image-generation call never is.
      if (!disposed) publish({ ...state, status: "read-error", images: [] })
    } finally { running = false }
  }
  return {
    refresh,
    event(type, event) {
      if (disposed) return
      const data = event.data
      if (type === "session.deleted") {
        stopped = true
        publish({ status: "interrupted", images: [] })
        disposed = true
        request?.abort()
        return
      }
      // Historical finished cards must not redownload their image on every later
      // execution in the chat. Reconnect/manual refresh still reconciles them.
      if (["complete", "error", "ambiguous"].includes(state.status) ||
        (state.status === "unavailable" && (state.toolID || finished))) return
      if (type === "session.execution.interrupted" || type === "session.execution.failed") {
        stopped = true
        if (["loading", "waiting", "running"].includes(state.status)) publish({ ...state, status: "interrupted" })
        return
      }
      if (type === "session.execution.succeeded") finished = true
      if (type.startsWith("session.tool.")) {
        if (state.messageID && data.assistantMessageID !== state.messageID) return
        if (state.toolID && data.id !== state.toolID) return
      }
      void refresh()
    },
    dispose() { disposed = true; request?.abort(); state = { status: "unavailable", images: [] } },
  }
}

const css = imageCss

function renderCard(root, state, refresh, document) {
  // Lifecycle/reconnect reads can repeat the running state. Keep the existing
  // compositor timeline rather than snapping particles and shimmer back to zero.
  if (state.status === "running" && root._imageStatus === "running" && root.children.length) return
  root._imageStatus = state.status
  root._imageCleanup?.()
  root._imageCleanup = undefined
  root.replaceChildren()
  root.setAttribute("data-fleet-image-card", "")
  root.setAttribute("aria-busy", String(state.status === "running"))
  if (state.status !== "complete") {
    const box = document.createElement("div")
    box.className = "fleet-image-wait"
    box.setAttribute("role", "status")
    const label = document.createElement("strong")
    label.textContent = ({ loading: "Reading image…", waiting: "Waiting to generate", running: "Creating image…",
      error: "Image generation failed", interrupted: "Generation interrupted",
      ambiguous: "Ambiguous image reference", unavailable: "Image unavailable", "read-error": "Could not read image" })[state.status]
    const detail = document.createElement("small")
    detail.textContent = state.status === "running" ? "It will appear here when ready" : "Generation is never retried automatically"
    box.append(label, detail)
    if (state.status === "read-error") {
      const retry = document.createElement("button")
      retry.textContent = "Read result again"
      retry.onclick = refresh
      box.append(retry)
    }
    root.append(box)
    if (state.status === "running" && document.createElementNS) root._imageCleanup = animateParticles(box, document)
    return
  }
  for (const [index, image] of state.images.entries()) {
    const zoom = document.createElement("button")
    zoom.className = "fleet-image-zoom"
    zoom.setAttribute("aria-label", `Open image ${index + 1} fullscreen`)
    zoom.setAttribute("aria-haspopup", "dialog")
    const img = document.createElement("img")
    img.src = image.uri
    img.alt = `Generated image ${index + 1}`
    img.loading = "lazy"
    img.decoding = "async"
    zoom.onclick = () => {
      root._imageCleanup?.()
      root._imageCleanup = openImageViewer(image, document)
    }
    const actions = document.createElement("div")
    actions.className = "fleet-image-actions"
    const label = document.createElement("span")
    label.textContent = "Image ready"
    img.onerror = () => { label.textContent = "Cannot display image — download original" }
    const download = document.createElement("a")
    download.textContent = "Download original"
    download.href = image.uri
    download.download = image.name
    actions.append(label, download)
    zoom.append(img)
    root.append(zoom, actions)
  }
}

function installInlineImages(ctx, { CodeBlock, Style, createKeyed, onCleanup, collectImages }) {
  if (!CodeBlock) return false
  ctx.add(Style, css)
  ctx.add(CodeBlock, {
    languages: ["fleet-image"],
    render(props) {
      const token = props.code.trim()
      if (!tokenPattern.test(token) || !props.session) return undefined
      const root = document.createElement("div")
      createKeyed(() => props.session, session => {
        if (!session) { root.replaceChildren(); return }
        const card = createCard({ token, collectImages,
          read: ({ cursor, signal }) => session.server.client.message.list({ sessionID: session.id,
            type: "assistant", limit: 20, ...(cursor ? { cursor } : { order: "desc" }) }, { signal }),
          readMessage: (messageID, signal) => session.server.client.session.message.get({ sessionID: session.id, messageID }, { signal }),
          onChange: state => renderCard(root, state, () => void card.refresh(), document),
        })
        renderCard(root, { status: "loading", images: [] }, () => void card.refresh(), document)
        onCleanup(() => { card.dispose(); root._imageCleanup?.(); root.replaceChildren() })
        createKeyed(() => session.server.data, data => {
          if (!data) return
          for (const type of ["session.tool.called", "session.tool.success", "session.tool.failed", "session.execution.succeeded",
            "session.execution.failed", "session.execution.interrupted", "session.deleted"]) {
            onCleanup(data.on(type, event => { if (event.data.sessionID === session.id) card.event(type, event) }))
          }
          // Also reconcile once after reconnecting; no timers or polling.
          void card.refresh()
        })
        if (!session.server.data) void card.refresh()
      })
      return root
    },
  })
  return true
}

const enabled = tools => [...imageTools].some(name => Object.hasOwn(tools ?? {}, name))
export { instructions, markers, findCard, createCard, renderCard, installInlineImages, css, enabled }
