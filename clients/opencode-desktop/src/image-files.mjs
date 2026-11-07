// Only saved inline raster results from the exact ExetRouter image tools.
// Never fetch a remote URL, read a local path, or accept active SVG/HTML content.
const imageTools = new Set(["exetrouter_generate_image", "exetrouter_edit_image"])
const extensions = { "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp" }

function imageFile(part) {
  if (part?.type !== "file" || !Object.hasOwn(extensions, part.mime) || typeof part.uri !== "string") return false
  const prefix = `data:${part.mime};base64,`
  if (!part.uri.startsWith(prefix)) return false
  const encoded = part.uri.slice(prefix.length)
  if (!encoded || encoded.length > 64 * 1024 * 1024 || encoded.length % 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(encoded)) return false
  try {
    const bytes = atob(encoded.slice(0, 64))
    if (part.mime === "image/png") return bytes.startsWith("\x89PNG\r\n\x1a\n")
    if (part.mime === "image/jpeg") return bytes.startsWith("\xff\xd8\xff")
    return bytes.startsWith("RIFF") && bytes.slice(8, 12) === "WEBP"
  } catch { return false }
}

function collectImages(messages) {
  const images = new Map()
  for (const message of messages ?? []) {
    if (message.type !== "assistant") continue
    for (const tool of message.content ?? []) {
      if (tool.type !== "tool" || !imageTools.has(tool.name) || tool.state?.status !== "completed") continue
      for (const [index, part] of (tool.state.content ?? []).entries()) {
        if (!imageFile(part)) continue
        const id = `${message.id}:${tool.id}:${index}`
        images.set(id, { id, uri: part.uri, mime: part.mime,
          created: tool.time?.completed ?? message.time?.created ?? 0,
          name: `image-${String(tool.id).replace(/[^a-zA-Z0-9_-]/g, "").slice(0, 80)}-${index + 1}.${extensions[part.mime]}` })
      }
    }
  }
  return [...images.values()].sort((a, b) => b.created - a.created || a.id.localeCompare(b.id))
}

export { imageFile, collectImages }
