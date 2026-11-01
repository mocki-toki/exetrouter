import assert from "node:assert/strict"
import { mkdtemp, writeFile, rm } from "node:fs/promises"
import { join } from "node:path"
import { tmpdir } from "node:os"
import { test } from "node:test"
import { DesktopClient } from "../src/desktop"
import { options } from "../src/options"

const result = { status: "copied", copied: true, retryCopy: false, login: null, profile: null, update: null }
async function fixture(source: string, run: (client: DesktopClient) => Promise<void>) {
  const root = await mkdtemp(join(tmpdir(), "exr-desktop-test-"))
  const path = join(root, "exr-fixture")
  await writeFile(path, `#!/usr/bin/env node\n${source}`, { mode: 0o700 })
  const client = new DesktopClient(options({ exrBinary: path, exrConfig: join(root, "profile.json") }))
  try { await run(client) } finally { await client.close(); await rm(root, { recursive: true, force: true }) }
}

test("private desktop pipe accepts only the typed protocol and never forwards extra secret fields", async () => {
  await fixture(`let count=0;require('node:readline').createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);if(!process.argv.includes('--config')||process.env.EXETROUTER_TOKEN)process.exit(1);process.stdout.write(JSON.stringify({id:r.id,value:{...${JSON.stringify(result)},status:String(++count),secret:'private-body-marker'}})+'\\n')})`, async (client) => {
    const first = client.request({ action: "token_create", name: "Synthetic", days: 90 })
    await assert.rejects(client.request({ action: "copy_retry" }), /already running/)
    const value = await first
    assert.equal(value.status, "1")
    assert.equal(value.copied, true)
    assert.doesNotMatch(JSON.stringify(value), /private-body-marker|secret/)
    assert.equal((await client.request({ action: "copy_retry" })).status, "2", "no mutation is automatically repeated")
    await assert.rejects(client.request({ action: "arbitrary_shell" } as never))
  })
})

test("native failures are redacted and do not retry a submitted mutation", async () => {
  await fixture(`let count=0;require('node:readline').createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);process.stdout.write(JSON.stringify(++count===1?{id:r.id,error:'private-body-marker'}:{id:r.id,value:{...${JSON.stringify(result)},status:String(count)}})+'\\n')})`, async (client) => {
    await assert.rejects(client.request({ action: "token_rotate", id: "tok_test", confirmed: true }), (error: Error) => /outcome may be unknown/.test(error.message) && !error.message.includes("private-body-marker"))
    assert.equal((await client.request({ action: "copy_retry" })).status, "2")
  })
})

test("desktop pipe bounds unterminated output and refuses requests after disposal", async () => {
  await fixture(`process.stdin.once('data',()=>process.stdout.write('x'.repeat(1048577)))`, async (client) => {
    await assert.rejects(client.request({ action: "profile" }), /unavailable or interrupted/)
    await client.close()
    await assert.rejects(client.request({ action: "profile" }), /closed/)
  })
})

test("an early native exit and broken stdin fail the request without an unhandled stream error", async () => {
  await fixture("process.stdin.destroy();process.exit(1)", async (client) => {
    await assert.rejects(client.request({action:"token_create",name:"x".repeat(60_000),days:90}), /unavailable or interrupted/)
    await assert.rejects(client.request({action:"profile"}), /unavailable or interrupted/)
  })
})

test("closing OAuth during a status read coalesces one cancellation instead of dropping it", async () => {
  await fixture(`let count=0;require('node:readline').createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);const value={...${JSON.stringify(result)},status:r.input.action+':'+(++count)};setTimeout(()=>process.stdout.write(JSON.stringify({id:r.id,value})+'\\n'),10)})`, async (client) => {
    const status = client.request({action:"login_status"})
    const cancel = client.cancelLogin()
    assert.equal(client.cancelLogin(),cancel)
    assert.equal((await status).status,"login_status:1")
    assert.equal((await cancel).status,"login_cancel:2")
  })
})
