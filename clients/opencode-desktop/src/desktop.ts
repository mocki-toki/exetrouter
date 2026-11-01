import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { homedir } from "node:os"
import { isAbsolute, join } from "node:path"
import { Schema } from "effect"
import { DesktopAction, DesktopResult } from "./schemas"
import type { Options } from "./options"

const failure = () => new Error("Native operation unavailable or interrupted. Use a matching exr build with desktop support. The outcome may be unknown; refresh metadata before repeating a mutation.")
const expand = (path: string) => path.startsWith("~/") ? join(homedir(), path.slice(2)) : path

/** A single private pipe, one request in flight, no renderer secrets or automatic mutation retries. */
export class DesktopClient {
  private child?: ChildProcessWithoutNullStreams
  private pending?: { id: number; resolve(value: DesktopResult): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> }
  private buffer = ""
  private sequence = 0
  private disposed = false
  private settled: Promise<void> = Promise.resolve()
  private cancellation?: Promise<DesktopResult>
  constructor(private options: Options) {}

  request(input: DesktopAction): Promise<DesktopResult> {
    if (this.disposed || this.pending) return Promise.reject(new Error("A native action is already running or the bridge is closed."))
    let frame: string
    try {
      input = Schema.decodeUnknownSync(DesktopAction)(input)
      frame = JSON.stringify({ id: ++this.sequence, input }) + "\n"
      if (Buffer.byteLength(frame) > 65_536) throw failure()
      this.start()
    } catch { return Promise.reject(failure()) }
    const promise = new Promise<DesktopResult>((resolve, reject) => {
      const id = this.sequence, child = this.child!
      const timer = setTimeout(() => { if (this.pending?.id === id) this.fail() }, input.action === "update_install" ? 600_000 : 55_000)
      this.pending = { id: this.sequence, resolve, reject, timer }
      child.stdin.write(frame, (error) => { if (error && this.child === child) this.fail() })
    })
    this.settled = promise.then(() => {}, () => {})
    return promise
  }

  /** One coalesced cancellation may wait for an in-flight read/status response. */
  cancelLogin(): Promise<DesktopResult> {
    this.cancellation ??= this.settled.then(() => this.request({ action: "login_cancel" })).finally(() => { this.cancellation = undefined })
    return this.cancellation
  }

  private start() {
    if (this.child) return
    const profile = this.options.exrConfig && expand(this.options.exrConfig)
    if (profile && !isAbsolute(profile)) throw failure()
    const env = { ...process.env }
    delete env.EXETROUTER_TOKEN
    // Do not force EXR_NO_UPDATE_CHECK here: only explicit update_install can check/install.
    const child = spawn(expand(this.options.exrBinary), [...(profile ? ["--config", profile] : []), "desktop"], {
      stdio: ["pipe", "pipe", "pipe"], shell: false, detached: process.platform !== "win32", env,
    })
    this.child = child
    for (const stream of [child.stdin, child.stdout, child.stderr]) stream.on("error", () => { if (this.child === child) this.fail() })
    child.stdout.setEncoding("utf8")
    child.stderr.resume() // Drain, but never forward arbitrary native errors.
    child.stdout.on("data", (chunk: string) => {
      if (this.child !== child) return
      this.buffer += chunk
      if (Buffer.byteLength(this.buffer) > 1_048_576) { this.fail(); return }
      const end = this.buffer.indexOf("\n")
      if (end < 0) return
      const line = this.buffer.slice(0, end)
      this.buffer = this.buffer.slice(end + 1)
      const pending = this.pending
      if (!pending || this.buffer) { this.fail(); return }
      try {
        const response = JSON.parse(line)
        if (response.id !== pending.id) throw failure()
        const value = response.error ? undefined : Schema.decodeUnknownSync(DesktopResult)(response.value)
        clearTimeout(pending.timer); this.pending = undefined
        if (value) pending.resolve(value)
        else pending.reject(failure())
      } catch { this.fail() }
    })
    child.on("error", () => { if (this.child === child) this.fail() })
    child.on("close", () => { if (this.child === child) this.fail() })
  }

  private fail() {
    const pending = this.pending
    this.pending = undefined
    if (pending) { clearTimeout(pending.timer); pending.reject(failure()) }
    const child = this.child
    this.child = undefined; this.buffer = ""
    if (child) {
      try { if (process.platform !== "win32" && child.pid) process.kill(-child.pid, "SIGKILL"); else child.kill("SIGKILL") } catch { /* Already exited. */ }
    }
  }

  async close() {
    this.disposed = true
    const child = this.child
    if (!child) return
    await new Promise<void>((resolve) => {
      const timer = setTimeout(() => { this.fail(); resolve() }, 2000)
      child.once("close", () => { clearTimeout(timer); resolve() })
      child.stdin.end()
    })
    this.fail()
  }
}
