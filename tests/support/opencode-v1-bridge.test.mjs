import {test} from "node:test";
import assert from "node:assert/strict";
import plugin from "../../clients/opencode-v1/exetrouter.mjs";
const encoder = new TextEncoder();
const event = (type, extra = {}) => `event: ${type}\r\ndata: ${JSON.stringify({type, ...extra})}\r\n\r\n`;
const completed = event("response.completed", {response: {status: "completed"}});
const created = event("response.created");
const message = /Automatic retries are disabled/;
async function configure(fetch, deadlines = {}) {
  const hooks = await plugin();
  const config = {provider: {exetrouter: {npm: "@ai-sdk/openai", options: {fetch, ...deadlines}}}};
  await hooks.config(config);
  return {hooks, config, fetch: config.provider.exetrouter.options.fetch};
}
function sse(chunks, cancel = () => {}) {
  return new Response(new ReadableStream({
    pull(controller) {
      const chunk = chunks.shift();
      if (chunk === undefined) controller.close();
      else controller.enqueue(encoder.encode(chunk));
    }, cancel,
  }), {headers: {"content-type": "text/event-stream"}});
}

test("successful fragmented SSE retains events and closes after completion", async () => {
  let cancelled = false;
  const chunks = [created.slice(0, 8), created.slice(8) + completed];
  const response = new Response(new ReadableStream({
    pull(controller) { const chunk = chunks.shift(); if (chunk) controller.enqueue(encoder.encode(chunk)); },
    cancel() { cancelled = true; },
  }), {headers: {"content-type": "text/event-stream"}});
  const bridge = await configure(async (_, init) => { assert.equal(init.redirect, "error"); return response; });
  assert.equal(await (await bridge.fetch("https://api.example.com/v1/responses")).text(), created + completed);
  assert.ok(cancelled);
});

test("failures discard arbitrary details and produce a fixed non-retryable Error", async () => {
  for (const fetch of [
    async () => { throw new Error("private-fixture 503 timeout"); },
    async () => new Response("private-fixture", {status: 503}),
    async () => new Response("private-fixture", {status: 429}),
    async () => new Response("private-fixture", {headers: {"content-type":"application/json"}}),
  ]) {
    const bridge = await configure(fetch);
    await assert.rejects(bridge.fetch("https://api.example.com/v1/responses"), (error) => {
      assert.match(error.message, message);
      assert.ok(!error.message.includes("private-fixture"));
      assert.equal(error.cause, undefined);
      assert.equal(error.statusCode, undefined);
      return true;
    });
  }
});

test("partial/invalid/error/incomplete streams end with an error", async () => {
  for (const content of ["", created, created + "data: invalid\n\n", event("error", {message:"private-fixture 503"}), event("response.failed"), event("response.incomplete"), "data: [DONE]\n\n", event("response.completed", {response:{status:"failed"}}), "x".repeat(1024 * 1024 + 1)]) {
    const bridge = await configure(async () => sse([content]));
    await assert.rejects((await bridge.fetch("https://api.example.com/v1/responses")).text(), message);
  }
});

test("bridge owns header and stream deadlines", async () => {
  const bridge = await configure(async (_, init) => new Promise((_, reject) => {
    init.signal.addEventListener("abort", () => reject(new Error("private-fixture timeout")), {once:true});
  }), {headerTimeout: 10});
  await assert.rejects(bridge.fetch("https://api.example.com/v1/responses"), message);
  const idle = await configure(async () => new Response(new ReadableStream({}), {headers:{"content-type":"text/event-stream"}}), {chunkTimeout:10});
  await assert.rejects((await idle.fetch("https://api.example.com/v1/responses")).text(), message);
  assert.equal(bridge.config.provider.exetrouter.options.headerTimeout, false);
  assert.equal(bridge.config.provider.exetrouter.options.chunkTimeout, 0);
  assert.equal(bridge.config.provider.exetrouter.options.timeout, false);
});

test("consumer cancellation cancels upstream and caller abort stays an abort", async () => {
  let cancelled = false;
  const bridge = await configure(async () => new Response(new ReadableStream({cancel() {cancelled = true;}}), {headers:{"content-type":"text/event-stream"}}));
  await (await bridge.fetch("https://api.example.com/v1/responses")).body.cancel();
  assert.ok(cancelled);
  const abort = new AbortController(); abort.abort();
  const failing = await configure(async () => {throw new Error("private-fixture");});
  await assert.rejects(failing.fetch("https://api.example.com/v1/responses", {signal:abort.signal}), {name:"AbortError"});
});

test("configuration and parameter hooks stay scoped and idempotent", async () => {
  const hooks = await plugin();
  const other = {provider:{other:{npm:"@ai-sdk/openai",options:{timeout:123}}}};
  await hooks.config(other);
  assert.deepEqual(other.provider.other.options, {timeout:123});
  const bridge = await configure(async () => sse([completed]));
  await bridge.hooks.config(bridge.config);
  assert.equal(bridge.fetch, bridge.config.provider.exetrouter.options.fetch);
  const output = {maxOutputTokens:10,temperature:1,options:{}};
  await hooks["chat.params"]({model:{providerID:"other"}},output);
  assert.equal(output.maxOutputTokens,10);
  await hooks["chat.params"]({model:{providerID:"exetrouter",api:{npm:"@ai-sdk/openai"}}},output);
  assert.equal(output.maxOutputTokens,undefined);
  assert.equal(output.temperature,undefined);
  assert.equal(output.options.store,false);
  await assert.rejects(hooks["chat.params"]({model:{providerID:"exetrouter",api:{npm:"other"}}},output), message);
});
