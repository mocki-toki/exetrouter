import assert = require("node:assert/strict")
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { Native, type TokenAction } from "../src/native"

test(
  "native adapter holds profile stdin open and uses only passive report commands",
  { skip: process.platform === "win32" },
  async (t) => {
    const dir = await mkdtemp(join(tmpdir(), "exr-vscode-test-"))
    t.after(() => rm(dir, { recursive: true, force: true }))
    const exe = join(dir, "fixture-exr")
    await writeFile(
      exe,
      `#!/usr/bin/env node
const args=process.argv.slice(2).join(' ');
if(args==='desktop') {
  process.stdin.on('data',chunk=>{
    const r=JSON.parse(chunk);
    if(r.input.action!=='profile')process.exit(2);
    console.log(JSON.stringify({id:r.id,value:{profile:{mode:'Standalone',clientVersion:'test',fields:[{label:'API endpoint',value:'http://127.0.0.1:8787/v1'}]}}}));
  });
}else if(args==='doctor --json') {console.log(JSON.stringify({schema_version:1}));process.exitCode=1;}
else if(['usage --period 30d --by model --json','usage --period 24h --json','usage --period 90d --by token --json','usage --period 7d --by user --json','quota-history 42 --period 7d --json','token list --json'].includes(args)) console.log('[]');
else process.exitCode=2;
`,
      { mode: 0o700 },
    )
    const n = new Native(exe)
    try {
      const first = n.profile(),
        second = n.profile()
      assert.equal(first, second)
      assert.deepEqual(await first, { mode: "Standalone", version: "test", base: "http://127.0.0.1:8787/v1" })
      assert.equal(n.profile(), first)
      assert.deepEqual(
        await Promise.all([n.report("doctor"), n.report("usage", "30d"), n.report("tokens")]),
        [{ schema_version: 1 }, [], []],
      )
      await Promise.all([
        n.report("usage", "24h", "total"),
        n.report("usage", "90d", "token"),
        n.report("usage", "7d", "user"),
      ])
      await n.history(42, "7d")
      assert.throws(() => n.history(-1, "7d"))
    } finally {
      n.dispose()
    }
    await assert.rejects(n.profile())
  },
)

test(
  "native clipboard recovery never reissues; uncertain writes cannot replay or migrate",
  { skip: process.platform === "win32" },
  async (t) => {
    const dir = await mkdtemp(join(tmpdir(), "exr-vscode-actions-"))
    t.after(() => rm(dir, { recursive: true, force: true }))
    const exe = join(dir, "exr"),
      trace = join(dir, "actions.jsonl")
    await writeFile(
      exe,
      `#!/usr/bin/env node
const fs=require('node:fs');const input=require('node:readline').createInterface({input:process.stdin});input.on('line',line=>{
const {id,input:a}=JSON.parse(line);fs.appendFileSync(${JSON.stringify(trace)},JSON.stringify(a)+'\\n');
if(a.action==='token_revoke')process.exit(4);
const status=({token_create:'copy_failed',copy_retry:'copied',token_rename:'saved',token_set_enabled:'saved',token_rotate:'copy_failed',copy_discard:'discarded'})[a.action];
const value=a.action==='profile'?{profile:{mode:'Remote Server'}}:{status,retryCopy:status==='copy_failed',secret:'must-not-cross-adapter'};
console.log(JSON.stringify({id,value}));});\n`,
      { mode: 0o700 },
    )
    const n = new Native(exe)
    try {
      const p = await n.profile(),
        create = { action: "token_create", name: "Fixture", days: 7 } as const
      assert.equal(await n.action(p, create), "copy_failed")
      assert.equal(n.canRetryCopy, true)
      await assert.rejects(n.action(p, create))
      assert.equal(await n.action(p, { action: "copy_retry" }), "copied")
      assert.equal(n.canRetryCopy, false)
      await assert.rejects(n.action(p, { action: "copy_retry" }))
      const acts: TokenAction[] = [
        { action: "token_rename", id: "fixture", name: "New" },
        { action: "token_set_enabled", id: "fixture", enabled: false, confirmed: true },
        { action: "token_set_enabled", id: "fixture", enabled: true, confirmed: true },
        { action: "token_rotate", id: "fixture", confirmed: true },
        { action: "copy_discard" },
      ]
      for (const a of acts) await n.action(p, a)
      const revoke = { action: "token_revoke", id: "fixture", confirmed: true } as const
      await assert.rejects(n.action(p, revoke), /outcome may be unknown/)
      await assert.rejects(n.action(p, revoke))
      await n.profile()
      await assert.rejects(n.action(p, revoke))
      const { readFile } = await import("node:fs/promises")
      assert.deepEqual(
        (await readFile(trace, "utf8"))
          .trim()
          .split("\n")
          .map((l) => JSON.parse(l)),
        [{ action: "profile" }, create, { action: "copy_retry" }, ...acts, revoke, { action: "profile" }],
      )
    } finally {
      n.dispose()
    }
  },
)
