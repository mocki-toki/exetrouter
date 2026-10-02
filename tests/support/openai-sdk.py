"""Opt-in SDK fixture. Uses only the local URL and synthetic router token."""
import os
import openai

assert openai.__version__ == "3.22.1", "Update the verified SDK matrix before changing the pin"

with openai.OpenAI(
    api_key=os.environ["EXETROUTER_TOKEN"],
    base_url=os.environ["EXETROUTER_TEST_URL"] + "/v1",
    max_retries=0,
    timeout=10,
) as client:
    messages = [{"role": "user", "content": "synthetic SDK fixture"}]
    if os.environ.get("EXETROUTER_TEST_MODE") == "failed":
        try:
            with client.chat.completions.create(model="gpt-test", messages=messages, stream=True) as stream:
                list(stream)
        except openai.APIError as error:
            assert "private-upstream-secret" not in str(error)
            print("SDK_ERROR_OK", openai.__version__)
        else:
            raise AssertionError("SDK accepted a failed stream")
    else:
        assert [model.id for model in client.models.list()] == ["gpt-test"]
        response = client.responses.create(model="gpt-test", input="synthetic SDK input", store=False)
        assert response.output_text == "EXETROUTER_SMOKE_OK"
        with client.responses.create(model="gpt-test", input="synthetic SDK input", store=False, stream=True) as stream:
            events = list(stream)
        assert any(event.type == "response.completed" for event in events)
        assert "".join(event.delta for event in events if event.type == "response.output_text.delta") == "EXETROUTER_SMOKE_OK"
        response = client.chat.completions.create(model="gpt-test", messages=messages)
        assert response.choices[0].message.content == "EXETROUTER_SMOKE_OK"
        assert response.usage.total_tokens == 12
        with client.chat.completions.create(model="gpt-test", messages=messages, stream=True, stream_options={"include_usage": True}) as stream:
            chunks = list(stream)
        assert "".join(chunk.choices[0].delta.content or "" for chunk in chunks if chunk.choices) == "EXETROUTER_SMOKE_OK"
        assert chunks[-2].choices[0].finish_reason == "stop"
        assert chunks[-1].choices == [] and chunks[-1].usage.total_tokens == 12
        tools = [{"type": "function", "function": {"name": "shell", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}}}]
        for streaming in (False, True):
            if not streaming:
                result = client.chat.completions.create(model="gpt-test", messages=messages, tools=tools)
                assert result.choices[0].finish_reason == "tool_calls"
                assistant = result.choices[0].message.model_dump(exclude_none=True)
            else:
                calls = {}
                with client.chat.completions.create(model="gpt-test", messages=messages, tools=tools, stream=True) as stream:
                    for chunk in stream:
                        for delta in chunk.choices[0].delta.tool_calls or []:
                            call = calls.setdefault(delta.index, {"id": "", "type": "function", "function": {"name": "", "arguments": ""}})
                            call["id"] += delta.id or ""
                            call["function"]["name"] += delta.function.name or ""
                            call["function"]["arguments"] += delta.function.arguments or ""
                assistant = {"role": "assistant", "content": None, "tool_calls": list(calls.values())}
            call = assistant["tool_calls"][0]
            assert call["function"]["name"] == "shell"
            # The fixture returns a synthetic tool result; it executes no command.
            transcript = messages + [assistant, {"role": "tool", "tool_call_id": call["id"], "content": "EXETROUTER_TOOL_OK"}]
            result = client.chat.completions.create(model="gpt-test", messages=transcript, tools=tools)
            assert result.choices[0].message.content == "EXETROUTER_SMOKE_OK"
        try:
            client.chat.completions.create(model="gpt-test", messages=messages, temperature=0.5)
        except openai.BadRequestError as error:
            assert error.body["param"] == "temperature"
        else:
            raise AssertionError("Unsupported parameter was silently accepted")
        print("SDK_ROUNDTRIP_OK", openai.__version__)
