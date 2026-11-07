// Pure geometry and compositor-driven motion; no model calls, timers or image cache.
export const shapeNames = ["circle", "star", "triangle", "square", "hexagon", "spiral", "infinity", "diamond"]
export const particleTiming = { hold: 1000, transition: 1400 }
const tau = Math.PI * 2
function polygon(t, sides, radius = 78, alternating = false) {
  const n = alternating ? sides * 2 : sides, edge = t * n, i = Math.floor(edge), f = edge - i
  const vertex = k => { const a = k / n * tau - Math.PI / 2, r = alternating && k % 2 ? radius * .43 : radius; return [Math.cos(a) * r, Math.sin(a) * r] }
  const a = vertex(i), b = vertex((i + 1) % n)
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
}
export function shapePoint(shape, t) {
  const a = t * tau
  switch (shapeNames[shape]) {
    case "circle": return [Math.cos(a) * 76, Math.sin(a) * 76]
    case "star": return polygon(t, 5, 86, true)
    case "triangle": return polygon(t, 3)
    case "square": return polygon((t + .125) % 1, 4, 87)
    case "hexagon": return polygon(t, 6)
    case "spiral": return [Math.cos(a * 2) * (12 + 66*t), Math.sin(a * 2) * (12 + 66*t)]
    case "infinity": return [88 * Math.cos(a) / (1 + Math.sin(a)**2), 88 * Math.sin(a) * Math.cos(a) / (1 + Math.sin(a)**2)]
    default: return polygon(t, 4)
  }
}
export function particleFrames(index, count = 72) {
  const frames = []
  const hold = particleTiming.hold / (particleTiming.hold + particleTiming.transition)
  for (let cycle = 0; cycle < shapeNames.length; cycle++) {
    const [x,y] = shapePoint((cycle * 7) % shapeNames.length, index/count)
    const figure = `translate(${x}px, ${y}px)`
    // Hold each shape, then morph directly to the next, without a scatter phase.
    frames.push({ offset: cycle/shapeNames.length, transform: figure, opacity: .9, easing: "linear" },
      { offset: (cycle + hold)/shapeNames.length, transform: figure, opacity: .9, easing: "cubic-bezier(.45,0,.15,1)" })
  }
  frames.push({ ...frames[0], offset: 1 })
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
    dot.style.transform = frames[0].transform
    if (dot.animate) animations.push(dot.animate(frames, {
      duration: shapeNames.length * (particleTiming.hold + particleTiming.transition), iterations: Infinity,
    }))
  }
  const start = document.timeline?.currentTime
  if (typeof start === "number") animations.forEach(animation => { animation.startTime = start })
  let visible = true
  const update = () => {
    box.classList.toggle("exr-image-still", !!motion?.matches || document.hidden || !visible)
    for (const animation of animations) {
      if (motion?.matches) { animation.pause(); animation.currentTime = 0 }
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
  dialog.tabIndex = -1
  const stage = document.createElement("div"), img = document.createElement("img")
  stage.className = "exr-image-stage"
  img.src = image.uri; img.alt = "Generated image"; img.draggable = false
  stage.append(img)
  let scale = 1, x = 0, y = 0, closed = false
  const points = new Map()
  const paint = () => {
    const rect = stage.getBoundingClientRect()
    x = Math.max(-rect.width*Math.max(0,scale-1)/2, Math.min(rect.width*Math.max(0,scale-1)/2, x))
    y = Math.max(-rect.height*Math.max(0,scale-1)/2, Math.min(rect.height*Math.max(0,scale-1)/2, y))
    img.style.transform = `translate(${x}px,${y}px) scale(${scale})`
  }
  const zoom = (factor, ax = 0, ay = 0) => {
    const next = Math.max(.25, Math.min(12, scale*factor)), ratio = next/scale
    x = ax + (x-ax)*ratio; y = ay + (y-ay)*ratio; scale = next; paint()
  }
  const close = () => {
    if (closed) return
    closed = true; points.clear(); document.defaultView?.removeEventListener("resize", paint); dialog.remove(); previous?.focus?.()
  }
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
  stage.ondblclick = () => { if (scale > 1) { scale=1;x=y=0;paint() } else zoom(2/scale) }
  dialog.oncancel = event => { event.preventDefault(); close() }
  dialog.onclose = close
  dialog.onkeydown = event => {
    if (event.key === "+" || event.key === "=") { event.preventDefault(); zoom(1.25) }
    if (event.key === "-") { event.preventDefault(); zoom(1/1.25) }
    if (event.key === "0") { event.preventDefault(); scale=1;x=y=0;paint() }
  }
  dialog.append(stage); document.body.append(dialog)
  document.defaultView?.addEventListener("resize", paint)
  dialog.showModal(); dialog.focus(); paint()
  return close
}

export const imageCss = `
[data-fleet-image-card]{box-sizing:border-box;width:300px;max-width:100%;margin:0;padding:0;line-height:0;color:var(--v2-text-text-base);border:0;border-radius:12px;overflow:hidden;background:var(--v2-background-bg-layer-01)}
[data-fleet-image-card][data-image-status=complete]{background:transparent}
[data-fleet-image-card] .fleet-image-wait{position:relative;isolation:isolate;box-sizing:border-box;width:100%;aspect-ratio:1;overflow:hidden;display:flex;flex-direction:column;align-items:center;justify-content:flex-end;padding:18px 12px;gap:5px;text-align:center;line-height:1.4;background:var(--v2-background-bg-layer-02)}
[data-fleet-image-card] .fleet-image-wait strong{font-size:13px;font-weight:500;z-index:1;max-width:100%;overflow-wrap:anywhere}
[data-fleet-image-card] small{font-size:10px;color:var(--v2-text-text-muted);z-index:1;max-width:100%}
[data-fleet-image-card][aria-busy=true] .fleet-image-wait::before{content:"";position:absolute;inset:0 -100%;background-image:linear-gradient(90deg,transparent,color-mix(in srgb,var(--v2-text-text-base,#888) 12%,transparent),transparent);background-size:calc(100% / 3) 100%;background-repeat:repeat-x;will-change:transform;animation:exr-image-shimmer 1.8s linear infinite;pointer-events:none;z-index:-1}
@keyframes exr-image-shimmer{from{transform:translate3d(0,0,0)}to{transform:translate3d(calc(100% / 3),0,0)}}
.exr-image-particles{position:absolute;top:40%;left:50%;width:0;height:0;pointer-events:none}
.exr-image-particles i{position:absolute;width:3px;height:3px;border-radius:50%;background:var(--v2-text-text-base);opacity:.7;will-change:transform,opacity}
.exr-image-still::before{animation-play-state:paused!important;opacity:0!important}
[data-fleet-image-card] .fleet-image-zoom{appearance:none;display:block;box-sizing:border-box;width:300px;max-width:100%;height:auto;min-height:0;border:0;padding:0!important;margin:0!important;line-height:0;background:transparent;cursor:zoom-in}
[data-fleet-image-card] .fleet-image-zoom:focus-visible{outline:2px solid currentColor;outline-offset:-2px}
[data-fleet-image-card] .fleet-image-zoom>img{display:block;width:100%!important;height:auto!important;max-width:100%!important;max-height:300px!important;min-width:0;min-height:0;margin:0!important;padding:0!important;border:0!important;border-radius:0;object-fit:contain}
[data-fleet-image-card] .fleet-image-error{display:block;padding:18px;font:13px/1.4 system-ui}
.exr-image-viewer{position:fixed;inset:0;box-sizing:border-box;width:100vw;height:100dvh;max-width:none;max-height:none;margin:0;border:0;padding:0;background:#111113;color:#eee;overflow:hidden}
.exr-image-viewer::backdrop{background:#111}
.exr-image-stage{position:absolute;inset:0;overflow:hidden;touch-action:none;cursor:grab}
.exr-image-stage:active{cursor:grabbing}
.exr-image-stage img{display:block;width:100%;height:100%;max-width:none;max-height:none;margin:0!important;padding:0;border:0;object-fit:contain;transform-origin:center;user-select:none;will-change:transform}
@media(prefers-reduced-motion:reduce){[data-fleet-image-card] .fleet-image-wait::before{animation:none!important;opacity:0!important}}
`
