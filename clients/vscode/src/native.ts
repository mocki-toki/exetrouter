import { execFile, spawn } from "node:child_process"
import { statSync } from "node:fs"
import { homedir } from "node:os"
import { isAbsolute, join, resolve } from "node:path"
import { baseUrl, record } from "./catalog"

export interface Profile {
  mode: string
  base?: string
  version?: string
  destination?: string
}
export type TokenAction =
  | { action: "token_create"; name: string; days: number }
  | { action: "token_rename"; id: string; name: string }
  | { action: "token_rotate" | "token_revoke"; id: string; confirmed: true }
  | { action: "token_set_enabled"; id: string; enabled: boolean; confirmed: true }
  | { action: "copy_retry" | "copy_discard" }
type ReplyStatus = "copied" | "copy_failed" | "clipboard_unavailable" | "discarded" | "saved" | "revoked"
export const periods = ["24h", "7d", "30d", "90d"] as const
export const groups = ["total", "model", "user", "token"] as const
export type Group = (typeof groups)[number]
type Report = "doctor" | "tokens" | "usage"
const unavailable = () =>
  new Error(
    "Native bridge unavailable. Refresh metadata before repeating an action; its outcome may be unknown.",
  )
export function profileStamp() {
  // Check file identity only. exr is the sole reader of private profile contents.
  const file =
    process.env.EXR_CONFIG ??
    join(process.env.XDG_CONFIG_HOME ?? join(homedir(), ".config"), "exr/config.json")
  try {
    const s = statSync(resolve(homedir(), file), { bigint: true, throwIfNoEntry: false })
    return s ? `${s.dev}:${s.ino}:${s.mtimeNs}:${s.size}` : "absent"
  } catch {
    return "unavailable"
  }
}
export class Native {
  private child?: ReturnType<typeof spawn>
  private profilePromise?: Promise<Profile>
  private activeProfile?: Profile
  private pending?: {
    id: number
    resolve: (value: Record<string, unknown>) => void
    reject: () => void
    timer: NodeJS.Timeout
  }
  private sequence = 0
  private copyPending = false
  private reads = new Map<string, Promise<unknown>>()
  private readonly lifetime = new AbortController()
  readonly stamp = profileStamp()
  get canRetryCopy() {
    return this.copyPending
  }
  constructor(private readonly exe: string) {
    if (exe !== "exr" && !isAbsolute(exe)) throw new Error("Use an absolute exr executable path.")
  }
  private close(child: ReturnType<typeof spawn>, graceful = false) {
    if (this.child !== child) return
    this.child = undefined
    this.activeProfile = undefined
    this.profilePromise = undefined
    this.copyPending = false
    const p = this.pending
    this.pending = undefined
    if (p) {
      clearTimeout(p.timer)
      p.reject()
    }
    if (graceful) child.stdin?.end()
    else child.kill()
  }
  private start() {
    const child = spawn(this.exe, ["desktop"], {
      cwd: homedir(),
      stdio: ["pipe", "pipe", "ignore"],
      windowsHide: true,
    })
    this.child = child
    child.once("error", () => this.close(child))
    child.once("close", () => this.close(child))
    child.stdin.on("error", () => this.close(child))
    let chunks: Buffer[] = [],
      bytes = 0
    child.stdout.on("data", (chunk: Buffer) => {
      if (this.child !== child) return
      const p = this.pending
      bytes += chunk.length
      if (!p || bytes > 8 * 1024 * 1024 + 1) return this.close(child)
      chunks.push(chunk)
      const nl = chunk.indexOf(10)
      if (nl < 0) return
      if (nl !== chunk.length - 1) return this.close(child)
      try {
        const frame = record(JSON.parse(Buffer.concat(chunks).toString()))
        chunks = []
        bytes = 0
        if (frame.id !== p.id || (!frame.value && !frame.error)) return this.close(child)
        clearTimeout(p.timer)
        this.pending = undefined
        if (frame.error) p.reject()
        else p.resolve(record(frame.value))
      } catch {
        this.close(child)
      }
    })
    return child
  }
  private send(input: TokenAction | { action: "profile" }) {
    const child = this.child
    if (!child?.stdin || this.pending || this.lifetime.signal.aborted) return Promise.reject(unavailable())
    const id = ++this.sequence,
      line = JSON.stringify({ id, input }) + "\n"
    if (Buffer.byteLength(line) > 65536) return Promise.reject(unavailable())
    return new Promise<Record<string, unknown>>((resolve, reject) => {
      this.pending = {
        id,
        resolve,
        reject: () => reject(unavailable()),
        timer: setTimeout(() => this.close(child), 20000),
      }
      child.stdin!.write(line, (err) => {
        if (err) this.close(child)
      })
    })
  }
  profile(): Promise<Profile> {
    if (this.lifetime.signal.aborted) return Promise.reject(unavailable())
    if (!this.profilePromise) {
      let child: ReturnType<typeof spawn>
      try {
        child = this.start()
      } catch {
        return Promise.reject(unavailable())
      }
      this.profilePromise = this.send({ action: "profile" })
        .then((reply) => {
          const p = record(reply.profile)
          if (typeof p.mode !== "string" || this.child !== child) throw unavailable()
          const api =
            Array.isArray(p.fields) &&
            p.fields.map(record).find((f) => ["API endpoint", "HTTP API URL"].includes(String(f.label)))
              ?.value
          return (this.activeProfile = {
            mode: p.mode,
            base: typeof api === "string" && api !== "Not configured" ? baseUrl(api) : undefined,
            version: typeof p.clientVersion === "string" ? p.clientVersion : undefined,
            ...(typeof p.destination === "string" ? { destination: p.destination } : {}),
          })
        })
        .catch(() => {
          this.close(child)
          throw unavailable()
        })
    }
    return this.profilePromise
  }
  /** A confirmed command is bound to the same live bridge that supplied the profile. Never reconnect or replay here. */
  async action(profile: Profile, input: TokenAction): Promise<ReplyStatus> {
    if (this.activeProfile !== profile || this.stamp !== profileStamp()) throw unavailable()
    if ((input.action === "copy_retry" || input.action === "copy_discard") && !this.copyPending)
      throw new Error("No native clipboard retry is available.")
    if ((input.action === "token_rotate" || input.action === "token_create") && this.copyPending)
      throw new Error("Resolve the pending clipboard copy first.")
    const r = await this.send(input)
    const status = r.status
    if (
      !["copied", "copy_failed", "clipboard_unavailable", "discarded", "saved", "revoked"].includes(
        String(status),
      )
    )
      throw unavailable()
    if (status === "copy_failed" && r.retryCopy === true) this.copyPending = true
    if (status === "copied" || status === "discarded") this.copyPending = false
    return status as ReplyStatus
  }
  report(kind: Report, period = "7d", group: Group = "model"): Promise<unknown> {
    if (!periods.some((p) => p === period) || !groups.includes(group))
      throw new Error("Unsupported usage selection.")
    return this.read(
      kind === "usage"
        ? ["usage", "--period", period, ...(group === "total" ? [] : ["--by", group])]
        : kind === "tokens"
          ? ["token", "list"]
          : ["doctor"],
      kind === "doctor",
    )
  }
  history(account: number, period: string) {
    if (!Number.isSafeInteger(account) || account <= 0 || !periods.some((p) => p === period))
      throw new Error("Invalid history selection.")
    return this.read(["quota-history", String(account), "--period", period])
  }
  private read(args: string[], allowNonzero = false) {
    const key = args.join(" "),
      existing = this.reads.get(key)
    if (existing) return existing
    if (this.reads.size >= 3) return Promise.reject(new Error("Native reports are busy."))
    const read = new Promise<unknown>((resolve, reject) => {
      execFile(
        this.exe,
        [...args, "--json"],
        {
          cwd: homedir(),
          timeout: 20000,
          maxBuffer: 8 * 1024 * 1024,
          signal: this.lifetime.signal,
          windowsHide: true,
        },
        (err, stdout) => {
          if (!err || (allowNonzero && typeof err.code === "number" && !err.killed))
            try {
              resolve(JSON.parse(stdout))
              return
            } catch {
              /* never echo native content */
            }
          reject(new Error("Native report unavailable."))
        },
      )
    }).finally(() => this.reads.delete(key))
    this.reads.set(key, read)
    return read
  }
  dispose() {
    this.lifetime.abort()
    if (this.child) this.close(this.child, !this.pending)
  }
}
