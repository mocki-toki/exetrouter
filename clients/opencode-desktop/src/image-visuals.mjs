// Pure geometry and compositor-driven motion; no model calls, timers or image cache.
export const shapeNames = ["circle", "heart", "star", "triangle", "square", "hexagon", "spiral", "infinity", "flower", "diamond"]
const tau = Math.PI * 2
function polygon(t, sides, radius = 78, alternating = false) {
  const n = alternating ? sides * 2 : sides, edge = t * n, i = Math.floor(edge), f = edge - i
  const vertex = k => { const a = k / n * tau - Math.PI / 2, r = alternating && k % 2 ? radius * .43 : radius; return [Math.cos(a) * r, Math.sin(a) * r] }
  const a = vertex(i), b = vertex((i + 1) % n)
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
}
export function shapePoint(shape, t) {
  const a = t * tau
  switch (shape) {
    case 0: return [Math.cos(a) * 76, Math.sin(a) * 76]
    case 1: return [4.6 * 16 * Math.sin(a) ** 3, -4.6 * (13 * Math.cos(a) - 5 * Math.cos(2*a) - 2 * Math.cos(3*a) - Math.cos(4*a))]
    case 2: return polygon(t, 5, 86, true)
    case 3: return polygon(t, 3)
    case 4: return polygon((t + .125) % 1, 4, 87)
    case 5: return polygon(t, 6)
    case 6: return [Math.cos(a * 2) * (12 + 66*t), Math.sin(a * 2) * (12 + 66*t)]
    case 7: return [88 * Math.cos(a) / (1 + Math.sin(a)**2), 88 * Math.sin(a) * Math.cos(a) / (1 + Math.sin(a)**2)]
    case 8: { const r = 55 + 24 * Math.cos(5*a); return [r * Math.cos(a), r * Math.sin(a)] }
    default: return polygon(t, 4)
  }
}
export function particleFrames(index, count = 72) {
  const scatter = cycle => {
    const angle = (index * 2.399963 + cycle * 1.71), r = 88 + 42 * ((index * 17 + cycle * 29) % 71) / 70
    return `translate(${Math.cos(angle)*r}px, ${Math.sin(angle)*r}px)`
  }
  const frames = []
  for (let cycle = 0; cycle < 10; cycle++) {
    const [x,y] = shapePoint((cycle * 7) % 10, index/count)
    const figure = `translate(${x}px, ${y}px)`
    frames.push({ offset: cycle/10, transform: scatter(cycle), opacity: .2, easing: "cubic-bezier(.45,0,.15,1)" },
      { offset: (cycle + .4)/10, transform: figure, opacity: .9, easing: "linear" },
      { offset: (cycle + .4 + 1/7)/10, transform: figure, opacity: .9, easing: "cubic-bezier(.45,0,.25,1)" },
      { offset: (cycle + .943)/10, transform: scatter((cycle+1)%10), opacity: .2, easing: "linear" })
  }
  frames.push({ offset: 1, transform: scatter(0), opacity: .2 })
  return frames
}
export function animateParticles(box, document) {
  const window = document.defaultView
  const layer = document.createElement("div")
  layer.className = "exr-image-particles"
  layer.setAttribute("aria-hidden", "true")
  box.prepend(layer)
  const motion = window?.matchMedia?.("(prefers-reduced-motion: reduce)")
  const animations = []
  for (let i=0; i<72; i++) {
    const dot = document.createElement("i")
    layer.append(dot)
    const frames = particleFrames(i)
    dot.style.transform = frames[1].transform
    if (dot.animate) animations.push(dot.animate(frames, { duration: 70000, iterations: Infinity }))
  }
  const start = document.timeline?.currentTime
  if (typeof start === "number") animations.forEach(animation => { animation.startTime = start })
  let visible = true
  const update = () => {
    box.classList.toggle("exr-image-still", !!motion?.matches || document.hidden || !visible)
    for (const animation of animations) {
      if (motion?.matches) { animation.pause(); animation.currentTime = 2800 }
      else if (document.hidden || !visible) animation.pause()
      else animation.play()
    }
  }
  const observer = window?.IntersectionObserver ? new window.IntersectionObserver(entries => { visible = entries[0]?.isIntersecting ?? true; update() }) : undefined
  observer?.observe(box)
  motion?.addEventListener("change", update)
  document.addEventListener("visibilitychange", update)
  update()
  return () => { observer?.disconnect(); motion?.removeEventListener("change", update); document.removeEventListener("visibilitychange", update); animations.forEach(a => a.cancel()); layer.remove() }
}

export function openImageViewer(image, document) {
  const previous = document.activeElement
  const dialog = document.createElement("dialog")
  dialog.className = "exr-image-viewer"
  dialog.setAttribute("aria-label", "Generated image viewer")
  const toolbar = document.createElement("div"), stage = document.createElement("div"), img = document.createElement("img")
  toolbar.className = "exr-image-viewer-toolbar"
  stage.className = "exr-image-stage"
  img.src = image.uri; img.alt = "Generated image"; img.draggable = false
  stage.append(img)
  const label = document.createElement("output")
  let scale = 1, x = 0, y = 0, closed = false
  const points = new Map()
  const paint = () => {
    const rect = stage.getBoundingClientRect()
    x = Math.max(-rect.width*Math.max(0,scale-1)/2, Math.min(rect.width*Math.max(0,scale-1)/2, x))
    y = Math.max(-rect.height*Math.max(0,scale-1)/2, Math.min(rect.height*Math.max(0,scale-1)/2, y))
    img.style.transform = `translate(${x}px,${y}px) scale(${scale})`
    label.textContent = `${Math.round(scale*100)}%`
  }
  const zoom = (factor, ax = 0, ay = 0) => {
    const next = Math.max(.25, Math.min(12, scale*factor)), ratio = next/scale
    x = ax + (x-ax)*ratio; y = ay + (y-ay)*ratio; scale = next; paint()
  }
  const close = () => {
    if (closed) return
    closed = true; points.clear(); document.defaultView?.removeEventListener("resize", paint); dialog.remove(); previous?.focus?.()
  }
  const button = (title, action) => { const b = document.createElement("button"); b.type = "button"; b.textContent = title; b.setAttribute("aria-label", title); b.onclick = action; toolbar.append(b) }
  button("Zoom out", () => zoom(1/1.25)); toolbar.append(label)
  button("Zoom in", () => zoom(1.25))
  button("Fit", () => { scale=1; x=y=0; paint() })
  button("Close", close)
  stage.addEventListener("wheel", event => {
    event.preventDefault()
    const rect = stage.getBoundingClientRect()
    zoom(Math.exp(-Math.max(-100,Math.min(100,event.deltaY))*.004), event.clientX-rect.left-rect.width/2, event.clientY-rect.top-rect.height/2)
  }, { passive: false })
  stage.onpointerdown = event => { if (event.button !== 0) return; points.set(event.pointerId, [event.clientX,event.clientY]); stage.setPointerCapture?.(event.pointerId) }
  stage.onpointermove = event => {
    const old = points.get(event.pointerId)
    if (!old) return
    const other = [...points].find(([id]) => id !== event.pointerId)?.[1]
    if (other) {
      const before = Math.hypot(old[0]-other[0],old[1]-other[1]), after = Math.hypot(event.clientX-other[0],event.clientY-other[1])
      const rect = stage.getBoundingClientRect()
      if (before > 2) zoom(after/before, (event.clientX+other[0])/2-rect.left-rect.width/2, (event.clientY+other[1])/2-rect.top-rect.height/2)
    } else { x+=event.clientX-old[0]; y+=event.clientY-old[1]; paint() }
    points.set(event.pointerId,[event.clientX,event.clientY])
  }
  const release = event => points.delete(event.pointerId)
  stage.onpointerup = release; stage.onpointercancel = release; stage.onlostpointercapture = release
  stage.ondblclick = () => { if (scale > 1) { scale=1;x=y=0;paint() } else zoom(2) }
  dialog.oncancel = event => { event.preventDefault(); close() }
  dialog.onclose = close
  dialog.onkeydown = event => {
    if (event.key === "+" || event.key === "=") { event.preventDefault(); zoom(1.25) }
    if (event.key === "-") { event.preventDefault(); zoom(1/1.25) }
    if (event.key === "0") { event.preventDefault(); scale=1;x=y=0;paint() }
  }
  dialog.append(toolbar, stage); document.body.append(dialog)
  document.defaultView?.addEventListener("resize", paint)
  dialog.showModal(); paint()
  return close
}

export const imageCss = `
[data-fleet-image-card]{width:300px;max-width:100%;color:var(--v2-text-text-base);border:1px solid var(--v2-border-border-muted);border-radius:12px;overflow:hidden;background:var(--v2-background-bg-layer-01)}
[data-fleet-image-card] .fleet-image-wait{position:relative;isolation:isolate;box-sizing:border-box;width:100%;height:300px;overflow:hidden;display:flex;flex-direction:column;align-items:center;justify-content:flex-end;padding:18px 12px;gap:5px;text-align:center;background:var(--v2-background-bg-layer-02)}
[data-fleet-image-card] .fleet-image-wait strong{font-size:13px;font-weight:500;z-index:1;max-width:100%;overflow-wrap:anywhere}
[data-fleet-image-card] small{font-size:10px;color:var(--v2-text-text-muted);z-index:1;max-width:100%}
[data-fleet-image-card][aria-busy=true] .fleet-image-wait::before{content:"";position:absolute;inset:0;background:linear-gradient(110deg,transparent 10%,var(--v2-background-bg-layer-03) 50%,transparent 90%);will-change:transform,opacity;animation:exr-image-shimmer 5s linear infinite;pointer-events:none;z-index:-1}
@keyframes exr-image-shimmer{0%{transform:translate3d(-110%,0,0);opacity:0}15%,85%{opacity:.65}100%{transform:translate3d(110%,0,0);opacity:0}}
.exr-image-particles{position:absolute;top:40%;left:50%;width:0;height:0;pointer-events:none}
.exr-image-particles i{position:absolute;width:3px;height:3px;border-radius:50%;background:var(--v2-text-text-base);opacity:.7;will-change:transform,opacity}
.exr-image-still::before{animation-play-state:paused!important;opacity:0!important}
[data-fleet-image-card] .fleet-image-zoom{display:block;width:100%;height:300px;border:0;padding:0;background:transparent;cursor:zoom-in}
[data-fleet-image-card] img{display:block;width:100%;height:100%;object-fit:contain}
[data-fleet-image-card] .fleet-image-actions{display:flex;flex-wrap:wrap;justify-content:space-between;gap:8px;padding:10px;font-size:11px}
[data-fleet-image-card] a{color:inherit;text-decoration:underline}
.exr-image-viewer{position:fixed;inset:0;box-sizing:border-box;width:100vw;height:100dvh;max-width:none;max-height:none;margin:0;border:0;padding:0;background:#111113;color:#eee;overflow:hidden}
.exr-image-viewer::backdrop{background:#111}
.exr-image-viewer-toolbar{position:absolute;top:0;left:0;right:0;height:56px;display:flex;align-items:center;justify-content:center;gap:12px;z-index:2;background:#111113ed}
.exr-image-viewer button{font:inherit;color:inherit;background:#27272b;border:1px solid #444;padding:7px 12px;border-radius:6px;cursor:pointer}
.exr-image-viewer output{min-width:48px;text-align:center;font:12px system-ui}
.exr-image-stage{position:absolute;inset:56px 0 0;overflow:hidden;touch-action:none;cursor:grab}
.exr-image-stage:active{cursor:grabbing}
.exr-image-stage img{display:block;width:100%;height:100%;object-fit:contain;transform-origin:center;user-select:none;will-change:transform}
@media(prefers-reduced-motion:reduce){[data-fleet-image-card] .fleet-image-wait::before{animation:none!important;opacity:0!important}}
`
