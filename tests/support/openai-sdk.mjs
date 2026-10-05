import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
const moduleURL = pathToFileURL(process.env.EXETROUTER_OPENAI_JS_MODULE);
const packageInfo = JSON.parse(await readFile(new URL('./package.json', moduleURL), 'utf8'));
assert.equal(packageInfo.version, '7.25.0', 'Update the verified SDK matrix before changing the pin');
const { default: OpenAI } = await import(moduleURL.href);
const client = new OpenAI({
  apiKey: process.env.EXETROUTER_TOKEN,
  baseURL: process.env.EXETROUTER_TEST_URL + '/v1',
  timeout: 10000,
});
const messages = [{ role: 'user', content: 'synthetic SDK fixture' }];
if (process.env.EXETROUTER_TEST_MODE === 'failed') {
  await assert.rejects(async () => {
    const stream = await client.chat.completions.create({ model: 'gpt-test', messages, stream: true });
    for await (const chunk of stream) void chunk;
  }, (error) => error instanceof OpenAI.APIError && !error.message.includes('private-upstream-secret'));
  console.log('SDK_ERROR_OK');
} else {
  const models = await client.models.list();
  assert.deepEqual(models.data.map((model) => model.id), ['gpt-test']);
  const response = await client.responses.create({ model: 'gpt-test', input: 'synthetic SDK input', store: false });
  assert.equal(response.output_text, 'EXETROUTER_SMOKE_OK');
  const responseStream = await client.responses.create({ model: 'gpt-test', input: 'synthetic SDK input', store: false, stream: true });
  let text = '', completed = false;
  for await (const event of responseStream) {
    if (event.type === 'response.output_text.delta') text += event.delta;
    if (event.type === 'response.completed') completed = true;
  }
  assert.equal(text, 'EXETROUTER_SMOKE_OK');
  assert.equal(completed, true);
  const result = await client.chat.completions.create({ model: 'gpt-test', messages });
  assert.equal(result.choices[0].message.content, 'EXETROUTER_SMOKE_OK');
  assert.equal(result.usage.total_tokens, 12);
  const stream = await client.chat.completions.create({ model: 'gpt-test', messages, stream: true, stream_options: { include_usage: true } });
  const chunks = [];
  for await (const chunk of stream) chunks.push(chunk);
  assert.equal(chunks.filter((chunk) => chunk.choices.length).map((chunk) => chunk.choices[0].delta.content ?? '').join(''), 'EXETROUTER_SMOKE_OK');
  assert.equal(chunks.at(-2).choices[0].finish_reason, 'stop');
  assert.deepEqual(chunks.at(-1).choices, []);
  assert.equal(chunks.at(-1).usage.total_tokens, 12);
  const tools = [{ type: 'function', function: { name: 'shell', parameters: { type: 'object', properties: { command: { type: 'string' } }, required: ['command'] } } }];
  for (const streaming of [false, true]) {
    let assistant;
    if (!streaming) {
      const result = await client.chat.completions.create({ model: 'gpt-test', messages, tools });
      assert.equal(result.choices[0].finish_reason, 'tool_calls');
      assistant = result.choices[0].message;
    } else {
      const calls = new Map();
      const stream = await client.chat.completions.create({ model: 'gpt-test', messages, tools, stream: true });
      for await (const chunk of stream) {
        for (const delta of chunk.choices[0].delta.tool_calls ?? []) {
          if (!calls.has(delta.index)) calls.set(delta.index, { id: '', type: 'function', function: { name: '', arguments: '' } });
          const call = calls.get(delta.index);
          call.id += delta.id ?? '';
          call.function.name += delta.function?.name ?? '';
          call.function.arguments += delta.function?.arguments ?? '';
        }
      }
      assistant = { role: 'assistant', content: null, tool_calls: [...calls.values()] };
    }
    const call = assistant.tool_calls[0];
    assert.equal(call.function.name, 'shell');
    // Return a synthetic result; never execute model-supplied commands.
    const transcript = [...messages, assistant, { role: 'tool', tool_call_id: call.id, content: 'EXETROUTER_TOOL_OK' }];
    const result = await client.chat.completions.create({ model: 'gpt-test', messages: transcript, tools });
    assert.equal(result.choices[0].message.content, 'EXETROUTER_SMOKE_OK');
  }
  await assert.rejects(client.chat.completions.create({ model: 'gpt-test', messages, temperature: 0.5 }), (error) => error instanceof OpenAI.BadRequestError && error.error.param === 'temperature');
  console.log('SDK_ROUNDTRIP_OK');
}
