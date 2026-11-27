(() => {
  const vscode = acquireVsCodeApi()
  const byId = (id) => document.getElementById(id)
  const controls = ["period", "group", "metric"].map(byId).filter(Boolean)
  const svg = byId("chart"), bounds = byId("plot"), detail = byId("cursor")
  const palette = ["#4b9ffa", "#36b78c", "#ce89f3", "#eaac46", "#ec789a", "#63bec4", "#f08760", "#a4af68"]
  const format = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 })
  const compact = new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 })
  const percent = new Intl.NumberFormat("en", { maximumFractionDigits: 1 })
  let chart, plot, times = [], selected = -1
  const hidden = new Set()
  const text = (tag, value) => {
    const node = document.createElement(tag)
    node.textContent = value
    return node
  }
  const formatValue = (value, short = false) => value === null || value === undefined ? "Unknown" :
    chart.metric === "percent" ? `${format.format(value)}%` :
      chart.metric === "usd" ? `$${short ? compact.format(value) : format.format(value)}` :
        (short ? compact : format).format(value)
  const date = (t) => new Date(t * 1000).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })
  const send = (refresh = false) => vscode.postMessage({ type: "chart", refresh, ...Object.fromEntries(controls.map((control) => [control.id, control.value])) })
  for (const control of controls) control.addEventListener("change", () => send())
  byId("refresh").addEventListener("click", () => send(true))

  function shape(name, attrs, value, target = svg) {
    const item = document.createElementNS("http://www.w3.org/2000/svg", name)
    for (const [key, val] of Object.entries(attrs)) item.setAttribute(key, String(val))
    if (value !== undefined) item.textContent = value
    target.append(item)
    return item
  }
  function renderQuotaMeters(meters, stale) {
    const grid = byId("quota-meters")
    if (!grid) return
    grid.replaceChildren()
    grid.hidden = !meters?.length
    for (const { label, remaining, caption, historical, hint } of meters ?? []) {
      const card = document.createElement("div"), heading = document.createElement("div")
      card.className = "quota-meter"
      card.dataset.tone = remaining === null || historical || stale ? "neutral" : remaining <= 20 ? "danger" : remaining <= 50 ? "warning" : "success"
      heading.className = "quota-meter-label"
      const name = text("span", label)
      if (hint) name.title = hint
      heading.append(name, text("strong", remaining === null ? "Unknown" : `${percent.format(remaining)}% left`))
      const track = shape("svg", { class: "quota-meter-track", viewBox: "0 0 100 4", preserveAspectRatio: "none", "aria-hidden": "true" }, undefined, card)
      if (remaining !== null) shape("rect", { width: remaining, height: 4 }, undefined, track)
      const footer = text("div", caption)
      footer.className = "quota-meter-caption"
      card.prepend(heading)
      card.append(footer)
      grid.append(card)
    }
  }
  function nearestIndex(sorted, target, key = (value) => value) {
    let low = 0, high = sorted.length
    while (low < high) {
      const mid = (low + high) >>> 1
      if (key(sorted[mid]) < target) low = mid + 1
      else high = mid
    }
    if (!low || low === sorted.length) return low ? low - 1 : 0
    return target - key(sorted[low - 1]) < key(sorted[low]) - target ? low - 1 : low
  }
  let marker
  function inspect(index) {
    if (!plot || !times.length || index < 0) return
    selected = Math.min(index, times.length - 1)
    const time = times[selected]
    marker.setAttribute("x1", String(plot.x(time)))
    marker.setAttribute("x2", String(plot.x(time)))
    marker.removeAttribute("visibility")
    detail.replaceChildren(text("strong", date(time)))
    for (const [i, series] of chart.series.entries()) {
      if (hidden.has(i) || !series.points.length) continue
      const index = nearestIndex(series.points, time, (p) => p[0]), p = series.points[index]
      const observed = p[0] === time ? p[1] : series.step && time >= series.points[0][0] && time <= series.points.at(-1)[0] ? (p[0] < time ? p[1] : series.points[index - 1]?.[1]) : null
      const row = text("span", `${series.label}: ${formatValue(observed)}`)
      detail.append(row)
    }
  }
  function draw() {
    if (!chart || bounds.hidden) return
    const width = Math.max(220, svg.clientWidth), height = svg.clientHeight || 230
    const left = 48, top = 13, right = width - 15, bottom = height - 30
    const visible = chart.series.filter((_, i) => !hidden.has(i))
    const max = chart.kind === "quota" ? 100 : visible.reduce((hi, s) => s.points.reduce((m, p) => Math.max(m, p[1] ?? 0), hi), 0)
    const scale = max ? Math.pow(10, Math.floor(Math.log10(max))) : 1
    const upper = chart.kind === "quota" ? 100 : (max ? Math.ceil(max / scale) * scale : 1)
    const x = (t) => left + (right - left) * ((t - chart.from) / (chart.to - chart.from))
    const y = (v) => bottom - (bottom - top) * v / upper
    plot = { x, left, right, width }
    svg.setAttribute("viewBox", `0 0 ${width} ${height}`)
    svg.replaceChildren()
    for (let i = 0; i <= 4; ++i) {
      const value = upper * i / 4, px = y(value)
      shape("line", { x1: left, x2: right, y1: px, y2: px, class: "grid" })
      shape("text", { x: left - 6, y: px + 4, "text-anchor": "end" }, formatValue(value, true))
    }
    const columns = width > 580 ? 5 : width > 360 ? 3 : 2
    const xLabel = new Intl.DateTimeFormat(undefined, chart.to - chart.from <= 90000 ? { hour: "2-digit", minute: "2-digit" } : { month: "short", day: "numeric" })
    for (let i = 0; i < columns; ++i) {
      const at = chart.from + (chart.to - chart.from) * i / (columns - 1)
      shape("text", { x: x(at), y: bottom + 19, "text-anchor": i === 0 ? "start" : i === columns - 1 ? "end" : "middle" }, xLabel.format(new Date(at * 1000)))
    }
    for (const [index, series] of chart.series.entries()) {
      if (hidden.has(index)) continue
      let line = "", prev
      for (const [at, value, gap] of series.points) {
        if (value === null) { prev = undefined; continue }
        const px = x(at).toFixed(2), py = y(value).toFixed(2)
        line += !prev || gap ? `M${px},${py}l.01,0` : series.step ? `H${px}V${py}` : `L${px},${py}`
        prev = true
      }
      shape("path", { d: line, fill: "none", stroke: palette[index % palette.length], "stroke-width": 1.65, "stroke-linecap": "round", "stroke-linejoin": "round" })
    }
    marker = shape("line", { y1: top, y2: bottom, class: "marker", visibility: "hidden" })
    if (selected >= 0) inspect(selected)
  }
  function render() {
    const legend = byId("legend")
    legend.replaceChildren()
    detail.replaceChildren()
    for (const [i, series] of chart.series.entries()) {
      const button = text("button", series.label)
      button.type = "button"
      button.setAttribute("aria-pressed", "true")
      const swatch = document.createElementNS("http://www.w3.org/2000/svg", "svg")
      const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle")
      circle.setAttribute("cx", "5"); circle.setAttribute("cy", "5"); circle.setAttribute("r", "4"); circle.setAttribute("fill", palette[i % palette.length]); swatch.append(circle); button.prepend(swatch)
      button.addEventListener("click", () => {
        if (hidden.delete(i)) button.setAttribute("aria-pressed", "true")
        else { hidden.add(i); button.setAttribute("aria-pressed", "false") }
        draw()
      })
      legend.append(button)
    }
    bounds.hidden = !chart.series.some((s) => s.points.some((p) => p[1] !== null))
    times = [...new Set(chart.series.flatMap((s) => s.points.map((p) => p[0])))].sort((a, b) => a - b)
    selected = -1
    const breakdown = byId("breakdown")
    breakdown.replaceChildren()
    if (chart.breakdown?.length) {
      const table = document.createElement("table"), header = document.createElement("tr")
      const caption = text("caption", "Period breakdown")
      for (const label of ["Group", chart.metric === "usd" ? "Est. USD" : chart.metric === "tokens" ? "Tokens" : "Requests", "Unknown usage"]) header.append(text("th", label))
      const thead = document.createElement("thead"), body = document.createElement("tbody")
      thead.append(header)
      for (const row of chart.breakdown) {
        const tr = document.createElement("tr")
        const group = text("td", row.label)
        if (chart.metric === "tokens") {
          const samples = [row.input, row.cached, row.output].map((v) => v === null ? "Unknown" : format.format(v))
          group.append(text("small", `Input ${samples[0]} · Cached ${samples[1]} · Output ${samples[2]}`))
        }
        tr.append(group, text("td", formatValue(row.value)), text("td", row.unknown === null ? "Unknown" : format.format(row.unknown)))
        body.append(tr)
      }
      table.append(caption, thead, body)
      breakdown.append(table)
    }
    draw()
  }
  svg.addEventListener("pointermove", (event) => {
    if (!chart || !plot || !times.length) return
    const rect = svg.getBoundingClientRect()
    const fraction = ((event.clientX - rect.left) * (plot.width / rect.width) - plot.left) / (plot.right - plot.left)
    inspect(nearestIndex(times, chart.from + (chart.to - chart.from) * fraction))
  })
  svg.addEventListener("keydown", (event) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return
    event.preventDefault()
    inspect(event.key === "Home" ? 0 : event.key === "End" ? times.length - 1 : Math.max(0, selected + (event.key === "ArrowRight" ? 1 : -1)))
  })
  new ResizeObserver(draw).observe(svg)
  window.addEventListener("message", ({ data }) => {
    if (data.type !== "chart") return
    byId("title").textContent = data.title
    byId("notice").textContent = data.notice || ""
    if (data.loading) return
    renderQuotaMeters(data.meters, data.staleMeters)
    chart = data.chart
    if (!chart) {
      bounds.hidden = true
      byId("range").replaceChildren()
      byId("legend").replaceChildren()
      byId("breakdown").replaceChildren()
      return
    }
    byId("range").textContent = `${date(chart.from)} – ${date(chart.to)}`
    hidden.clear()
    render()
    byId("notice").textContent = [data.notice, bounds.hidden ? "No numeric observations for this selection." : "", chart.summary].filter(Boolean).join(" ")
  })
  send()
})()
