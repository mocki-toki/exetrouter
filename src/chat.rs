//! The text/image-input/function-call subset of Chat Completions. Validation is deliberately
//! explicit: fields without an established Codex-backend mapping are rejected.
use crate::{usage::Counters, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug)]
pub(crate) struct InvalidRequest {
    pub param: String,
    pub message: &'static str,
}

fn invalid(param: impl Into<String>, message: &'static str) -> InvalidRequest {
    InvalidRequest {
        param: param.into(),
        message,
    }
}

fn fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> std::result::Result<(), InvalidRequest> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            let parameter =
                if key.len() <= 64 && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    if path == "request" {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    }
                } else {
                    path.into()
                };
            return Err(invalid(
                parameter,
                "unsupported field; see the Chat Completions contract",
            ));
        }
    }
    Ok(())
}

fn object<'a>(
    value: &'a Value,
    path: &str,
) -> std::result::Result<&'a Map<String, Value>, InvalidRequest> {
    value
        .as_object()
        .ok_or_else(|| invalid(path, "expected an object"))
}

fn string<'a>(
    value: Option<&'a Value>,
    path: &str,
) -> std::result::Result<&'a str, InvalidRequest> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(path, "expected a string"))
}

fn boolean(
    object: &Map<String, Value>,
    key: &str,
    default: bool,
    path: &str,
) -> std::result::Result<bool, InvalidRequest> {
    match object.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(invalid(path, "expected a boolean")),
    }
}

fn identifier(value: Option<&Value>, path: &str) -> std::result::Result<String, InvalidRequest> {
    let value = string(value, path)?;
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(invalid(path, "invalid identifier"));
    }
    Ok(value.into())
}

fn name(value: Option<&Value>, path: &str) -> std::result::Result<String, InvalidRequest> {
    let value = string(value, path)?;
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid(path, "invalid function name"));
    }
    Ok(value.into())
}

fn content_parts(
    value: &Value,
    assistant: bool,
    user_images: bool,
    path: &str,
) -> std::result::Result<Vec<Value>, InvalidRequest> {
    let text_part = |text: &str| {
        if assistant {
            json!({"type":"output_text","text":text,"annotations":[]})
        } else {
            json!({"type":"input_text","text":text})
        }
    };
    match value {
        Value::String(text) => Ok(vec![text_part(text)]),
        Value::Array(parts) if !parts.is_empty() => {
            let mut content = Vec::with_capacity(parts.len());
            for (index, part) in parts.iter().enumerate() {
                let path = format!("{path}[{index}]");
                let part = object(part, &path)?;
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        fields(part, &["type", "text"], &path)?;
                        content.push(text_part(string(
                            part.get("text"),
                            &format!("{path}.text"),
                        )?));
                    }
                    Some("image_url") if user_images => {
                        fields(part, &["type", "image_url"], &path)?;
                        let image_path = format!("{path}.image_url");
                        let image = object(
                            part.get("image_url")
                                .ok_or_else(|| invalid(&image_path, "image_url is required"))?,
                            &image_path,
                        )?;
                        fields(image, &["url", "detail"], &image_path)?;
                        let url = string(image.get("url"), &format!("{image_path}.url"))?;
                        if url.is_empty() || url.chars().any(char::is_control) {
                            return Err(invalid(
                                format!("{image_path}.url"),
                                "expected a nonempty image reference without control characters",
                            ));
                        }
                        // References remain transient. The router never fetches URLs or decodes images.
                        let mut input = json!({"type":"input_image","image_url":url});
                        if let Some(detail) = image.get("detail") {
                            let detail = string(Some(detail), &format!("{image_path}.detail"))?;
                            if !matches!(detail, "auto" | "low" | "high" | "original") {
                                return Err(invalid(
                                    format!("{image_path}.detail"),
                                    "unsupported image detail",
                                ));
                            }
                            input["detail"] = json!(detail);
                        }
                        content.push(input);
                    }
                    Some("image_url") => {
                        return Err(invalid(
                            format!("{path}.type"),
                            "image content is supported only in user messages",
                        ))
                    }
                    _ => {
                        return Err(invalid(
                            format!("{path}.type"),
                            "unsupported content part type",
                        ))
                    }
                }
            }
            Ok(content)
        }
        _ => Err(invalid(
            path,
            "expected a string or a nonempty content-part array",
        )),
    }
}

pub(crate) struct Request {
    pub payload: Value,
    pub streaming: bool,
    pub include_usage: bool,
}

pub(crate) fn prepare(value: Value) -> std::result::Result<Request, InvalidRequest> {
    let request = object(&value, "request")?;
    fields(
        request,
        &[
            "model",
            "messages",
            "stream",
            "stream_options",
            "n",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "store",
            "reasoning_effort",
            "response_format",
            "prompt_cache_key",
        ],
        "request",
    )?;
    if request
        .get("n")
        .filter(|v| !v.is_null())
        .is_some_and(|v| v.as_u64() != Some(1))
    {
        return Err(invalid("n", "only n=1 is supported"));
    }
    if boolean(request, "store", false, "store")? {
        return Err(invalid("store", "only store=false is supported"));
    }
    let streaming = boolean(request, "stream", false, "stream")?;
    let include_usage = match request.get("stream_options").filter(|v| !v.is_null()) {
        None => false,
        Some(options) => {
            if !streaming {
                return Err(invalid(
                    "stream_options",
                    "stream_options requires stream=true",
                ));
            }
            let options = object(options, "stream_options")?;
            fields(
                options,
                &["include_usage", "include_obfuscation"],
                "stream_options",
            )?;
            if boolean(
                options,
                "include_obfuscation",
                false,
                "stream_options.include_obfuscation",
            )? {
                return Err(invalid(
                    "stream_options.include_obfuscation",
                    "stream obfuscation is not implemented",
                ));
            }
            boolean(
                options,
                "include_usage",
                false,
                "stream_options.include_usage",
            )?
        }
    };
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid("messages", "expected a nonempty messages array"))?;
    let mut input = Vec::new();
    let mut calls = HashSet::new();
    let mut pending = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        let path = format!("messages[{index}]");
        let message = object(message, &path)?;
        let role = string(message.get("role"), &format!("{path}.role"))?;
        if role == "tool" {
            fields(message, &["role", "content", "tool_call_id"], &path)?;
            let id = identifier(message.get("tool_call_id"), &format!("{path}.tool_call_id"))?;
            if !pending.remove(&id) {
                return Err(invalid(
                    format!("{path}.tool_call_id"),
                    "tool result must reference an unanswered preceding tool call",
                ));
            }
            let parts = content_parts(
                message
                    .get("content")
                    .ok_or_else(|| invalid(&path, "content is required"))?,
                false,
                false,
                &format!("{path}.content"),
            )?;
            let output: String = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            input.push(json!({"type":"function_call_output","call_id":id,"output":output}));
            continue;
        }
        if !pending.is_empty() {
            return Err(invalid(
                &path,
                "all preceding tool calls require a result before the next message",
            ));
        }
        if !matches!(role, "system" | "developer" | "user" | "assistant") {
            return Err(invalid(format!("{path}.role"), "unsupported message role"));
        }
        fields(
            message,
            if role == "assistant" {
                &["role", "content", "tool_calls", "refusal"]
            } else {
                &["role", "content"]
            },
            &path,
        )?;
        let mut content = match message.get("content").filter(|v| !v.is_null()) {
            Some(content) => content_parts(
                content,
                role == "assistant",
                role == "user",
                &format!("{path}.content"),
            )?,
            None if role == "assistant" => Vec::new(),
            _ => return Err(invalid(format!("{path}.content"), "content is required")),
        };
        if let Some(refusal) = message.get("refusal").filter(|v| !v.is_null()) {
            content.push(json!({"type":"refusal","refusal":string(Some(refusal), &format!("{path}.refusal"))?}));
        }
        if !content.is_empty() {
            let mut item = json!({"type":"message","role":if role == "system" {"developer"} else {role},"content":content});
            if role == "assistant" {
                item["status"] = json!("completed");
            }
            input.push(item);
        }
        if let Some(tools) = message.get("tool_calls").filter(|v| !v.is_null()) {
            let tools = tools
                .as_array()
                .ok_or_else(|| invalid(format!("{path}.tool_calls"), "expected an array"))?;
            for (index, tool) in tools.iter().enumerate() {
                let path = format!("{path}.tool_calls[{index}]");
                let tool = object(tool, &path)?;
                fields(tool, &["id", "type", "function"], &path)?;
                if tool.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(invalid(&path, "only function tool calls are supported"));
                }
                let id = identifier(tool.get("id"), &format!("{path}.id"))?;
                if !calls.insert(id.clone()) {
                    return Err(invalid(&path, "duplicate tool call ID"));
                }
                pending.insert(id.clone());
                let function = object(
                    tool.get("function")
                        .ok_or_else(|| invalid(&path, "function is required"))?,
                    &format!("{path}.function"),
                )?;
                fields(
                    function,
                    &["name", "arguments"],
                    &format!("{path}.function"),
                )?;
                input.push(json!({"type":"function_call","call_id":id,"name":name(function.get("name"), &format!("{path}.function.name"))?,"arguments":string(function.get("arguments"), &format!("{path}.function.arguments"))?}));
            }
        }
        if role == "assistant" && content.is_empty() && pending.is_empty() {
            return Err(invalid(
                &path,
                "assistant message requires content, refusal or tool calls",
            ));
        }
    }
    if !pending.is_empty() {
        return Err(invalid(
            "messages",
            "all tool calls require a result before requesting the next response",
        ));
    }
    // Codex requires this field even when instructions live in input messages.
    let mut payload = json!({"model":request.get("model").ok_or_else(|| invalid("model", "model is required"))?,"instructions":"","input":input,"store":false,"stream":true});
    if let Some(value) = request.get("prompt_cache_key") {
        payload["prompt_cache_key"] = value.clone();
    }
    let mut tool_names = HashSet::new();
    if let Some(tools) = request.get("tools").filter(|v| !v.is_null()) {
        let tools = tools
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or_else(|| invalid("tools", "expected an array with at most 128 functions"))?;
        let mut translated = Vec::new();
        for (index, tool) in tools.iter().enumerate() {
            let path = format!("tools[{index}]");
            let tool = object(tool, &path)?;
            fields(tool, &["type", "function"], &path)?;
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return Err(invalid(&path, "only function tools are supported"));
            }
            let function = object(
                tool.get("function")
                    .ok_or_else(|| invalid(&path, "function is required"))?,
                &format!("{path}.function"),
            )?;
            fields(
                function,
                &["name", "description", "parameters", "strict"],
                &format!("{path}.function"),
            )?;
            let name = name(function.get("name"), &format!("{path}.function.name"))?;
            if !tool_names.insert(name.clone()) {
                return Err(invalid(&path, "duplicate function name"));
            }
            // Chat defaults to non-strict tools; Responses has a different
            // default. Preserve the caller's semantics across the conversion.
            let mut result = json!({"type":"function","name":name,"strict":false});
            for key in ["description", "parameters", "strict"] {
                if let Some(value) = function.get(key).filter(|v| !v.is_null()) {
                    let valid = match key {
                        "description" => value.is_string(),
                        "parameters" => value.is_object(),
                        _ => value.is_boolean(),
                    };
                    if !valid {
                        return Err(invalid(
                            format!("{path}.function.{key}"),
                            "invalid function option",
                        ));
                    }
                    result[key] = value.clone();
                }
            }
            translated.push(result);
        }
        payload["tools"] = json!(translated);
    }
    if let Some(choice) = request.get("tool_choice").filter(|v| !v.is_null()) {
        payload["tool_choice"] = match choice {
            Value::String(choice) if matches!(choice.as_str(), "auto" | "none" | "required") => {
                if choice == "required" && tool_names.is_empty() {
                    return Err(invalid(
                        "tool_choice",
                        "required needs at least one function",
                    ));
                }
                json!(choice)
            }
            Value::Object(choice) => {
                fields(choice, &["type", "function"], "tool_choice")?;
                if choice.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(invalid("tool_choice", "only a function can be selected"));
                }
                let function = object(
                    choice
                        .get("function")
                        .ok_or_else(|| invalid("tool_choice.function", "function is required"))?,
                    "tool_choice.function",
                )?;
                fields(function, &["name"], "tool_choice.function")?;
                let name = name(function.get("name"), "tool_choice.function.name")?;
                if !tool_names.contains(&name) {
                    return Err(invalid(
                        "tool_choice.function.name",
                        "selected function is not in tools",
                    ));
                }
                json!({"type":"function","name":name})
            }
            _ => return Err(invalid("tool_choice", "invalid function choice")),
        };
    }
    if request
        .get("parallel_tool_calls")
        .is_some_and(|v| !v.is_null())
    {
        payload["parallel_tool_calls"] = json!(boolean(
            request,
            "parallel_tool_calls",
            true,
            "parallel_tool_calls"
        )?);
    }
    if let Some(effort) = request.get("reasoning_effort").filter(|v| !v.is_null()) {
        let effort = string(Some(effort), "reasoning_effort")?;
        if !matches!(
            effort,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ) {
            return Err(invalid("reasoning_effort", "unsupported reasoning effort"));
        }
        payload["reasoning"] = json!({"effort":effort});
    }
    if let Some(format) = request.get("response_format").filter(|v| !v.is_null()) {
        let format = object(format, "response_format")?;
        fields(format, &["type", "json_schema"], "response_format")?;
        payload["text"] = match format.get("type").and_then(Value::as_str) {
            Some("text" | "json_object") if !format.contains_key("json_schema") => {
                json!({"format":format})
            }
            Some("json_schema") => {
                let schema = object(
                    format.get("json_schema").ok_or_else(|| {
                        invalid(
                            "response_format.json_schema",
                            "schema definition is required",
                        )
                    })?,
                    "response_format.json_schema",
                )?;
                fields(
                    schema,
                    &["name", "description", "schema", "strict"],
                    "response_format.json_schema",
                )?;
                name(schema.get("name"), "response_format.json_schema.name")?;
                if !schema.get("schema").is_some_and(Value::is_object) {
                    return Err(invalid(
                        "response_format.json_schema.schema",
                        "expected a schema object",
                    ));
                }
                if schema
                    .get("description")
                    .is_some_and(|v| !v.is_null() && !v.is_string())
                {
                    return Err(invalid(
                        "response_format.json_schema.description",
                        "expected a string",
                    ));
                }
                let strict = boolean(
                    schema,
                    "strict",
                    false,
                    "response_format.json_schema.strict",
                )?;
                let mut translated = schema.clone();
                translated.retain(|_, v| !v.is_null());
                translated.insert("type".into(), json!("json_schema"));
                translated.insert("strict".into(), json!(strict));
                json!({"format":translated})
            }
            _ => return Err(invalid("response_format", "unsupported response format")),
        };
    }
    Ok(Request {
        payload,
        streaming,
        include_usage,
    })
}

struct Metadata {
    id: String,
    model: String,
    created: i64,
}

impl Metadata {
    fn read(response: &Value) -> Result<Self> {
        let id = upstream_string(response.get("id"))?;
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err("invalid response ID".into());
        }
        let model = upstream_string(response.get("model"))?;
        if model.is_empty() || model.len() > 128 || model.chars().any(char::is_control) {
            return Err("invalid response model".into());
        }
        let created = response
            .get("created_at")
            .and_then(Value::as_i64)
            .filter(|v| *v >= 0)
            .ok_or("invalid response timestamp")?;
        Ok(Self {
            id: format!("chatcmpl-{id}"),
            model: model.into(),
            created,
        })
    }
    fn chunk(&self, delta: Value, finish: Value, include_usage: bool) -> Value {
        let mut result = json!({"id":self.id,"object":"chat.completion.chunk","created":self.created,"model":self.model,"choices":[{"index":0,"delta":delta,"finish_reason":finish,"logprobs":null}]});
        if include_usage {
            result["usage"] = Value::Null;
        }
        result
    }
}

fn upstream_string(value: Option<&Value>) -> Result<&str> {
    value
        .and_then(Value::as_str)
        .ok_or("invalid upstream string".into())
}

fn function(item: &Value) -> Result<Value> {
    if item.get("namespace").is_some_and(|v| !v.is_null()) {
        return Err("namespaced functions cannot be represented in Chat Completions".into());
    }
    let id = identifier(item.get("call_id"), "call_id")
        .map_err(|_| "invalid upstream function call ID")?;
    let name = name(item.get("name"), "name").map_err(|_| "invalid upstream function name")?;
    Ok(
        json!({"id":id,"type":"function","function":{"name":name,"arguments":upstream_string(item.get("arguments"))?}}),
    )
}

fn finish(response: &Value, has_tools: bool) -> Result<&'static str> {
    match response.get("status").and_then(Value::as_str) {
        Some("completed") => Ok(if has_tools { "tool_calls" } else { "stop" }),
        Some("incomplete") => match response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("max_output_tokens") => Ok("length"),
            Some("content_filter") => Ok("content_filter"),
            _ => Err("unsupported incomplete response reason".into()),
        },
        _ => Err("upstream response failed".into()),
    }
}

fn usage(response: &Value) -> Result<Value> {
    let counters = Counters::from_json(response.get("usage"))?;
    if counters.input.is_none()
        && counters.output.is_none()
        && counters.cached_input.is_none()
        && counters.reasoning_output.is_none()
    {
        return Ok(Value::Null);
    }
    let total = match (counters.input, counters.output) {
        (Some(input), Some(output)) => {
            Some(input.checked_add(output).ok_or("upstream usage overflow")?)
        }
        _ => None,
    };
    Ok(
        json!({"prompt_tokens":counters.input,"completion_tokens":counters.output,"total_tokens":total,"prompt_tokens_details":{"cached_tokens":counters.cached_input},"completion_tokens_details":{"reasoning_tokens":counters.reasoning_output}}),
    )
}

pub(crate) fn completion(response: &Value) -> Result<Value> {
    let meta = Metadata::read(response)?;
    let mut content = String::new();
    let mut has_text = false;
    let mut refusal = String::new();
    let mut calls = Vec::new();
    let mut call_ids = HashSet::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .ok_or("upstream output missing")?
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") if item.get("role").and_then(Value::as_str) == Some("assistant") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or("message content missing")?
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            content.push_str(upstream_string(part.get("text"))?);
                            has_text = true;
                        }
                        Some("refusal") => refusal.push_str(upstream_string(part.get("refusal"))?),
                        _ => return Err("unsupported upstream message content".into()),
                    }
                }
            }
            Some("function_call") => {
                let call = function(item)?;
                if !call_ids.insert(upstream_string(call.get("id"))?.to_owned()) {
                    return Err("duplicate upstream function call ID".into());
                }
                calls.push(call);
            }
            Some("reasoning") => {}
            _ => return Err("unsupported upstream output item".into()),
        }
    }
    let mut message = json!({"role":"assistant","content":if has_text {json!(content)} else if calls.is_empty() && refusal.is_empty() {json!("")} else {Value::Null},"refusal":if refusal.is_empty() {Value::Null} else {json!(refusal)}});
    let reason = finish(response, !calls.is_empty())?;
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    Ok(
        json!({"id":meta.id,"object":"chat.completion","created":meta.created,"model":meta.model,"choices":[{"index":0,"message":message,"finish_reason":reason,"logprobs":null}],"usage":usage(response)?}),
    )
}

#[derive(Default)]
struct Sent {
    bytes: usize,
    digest: Sha256,
}
impl Sent {
    fn append(&mut self, text: &str) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .filter(|v| *v <= crate::payload::PROJECTED_OUTPUT_BYTES)
            .ok_or("translated output too large")?;
        self.digest.update(text.as_bytes());
        Ok(())
    }
    fn reconcile(&mut self, text: &str) -> Result<Option<String>> {
        if self.bytes == 0 {
            self.append(text)?;
            return Ok(Some(text.into()));
        }
        if self.bytes != text.len()
            || self.digest.clone().finalize() != Sha256::digest(text.as_bytes())
        {
            return Err("upstream terminal output disagrees with streamed deltas".into());
        }
        Ok(None)
    }
}

enum Item {
    Message {
        id: String,
        parts: BTreeMap<u64, (String, Sent)>,
    },
    Function {
        id: String,
        call: Value,
        index: usize,
        arguments: Sent,
    },
    Reasoning,
}

pub(crate) struct Stream {
    meta: Option<Metadata>,
    items: BTreeMap<u64, Item>,
    calls: HashSet<String>,
    include_usage: bool,
    output_bytes: usize,
    content_parts: usize,
}

fn bounded_add(total: &mut usize, count: usize, limit: usize) -> Result<()> {
    *total = total
        .checked_add(count)
        .filter(|value| *value <= limit)
        .ok_or("translated output exceeds limits")?;
    Ok(())
}

impl Stream {
    pub fn new(include_usage: bool) -> Self {
        Self {
            meta: None,
            items: BTreeMap::new(),
            calls: HashSet::new(),
            include_usage,
            output_bytes: 0,
            content_parts: 0,
        }
    }
    fn chunk(&self, delta: Value) -> Result<Vec<u8>> {
        Ok(data(
            &self.meta.as_ref().ok_or("missing response.created")?.chunk(
                delta,
                Value::Null,
                self.include_usage,
            ),
        ))
    }
    pub fn event(&mut self, event: &Value) -> Result<Vec<Vec<u8>>> {
        let mut chunks = Vec::new();
        match event
            .get("type")
            .and_then(Value::as_str)
            .ok_or("missing event type")?
        {
            "response.created" => {
                if self.meta.is_some() {
                    return Err("duplicate response.created".into());
                }
                self.meta = Some(Metadata::read(
                    event.get("response").ok_or("response missing")?,
                )?);
                chunks.push(self.chunk(json!({"role":"assistant","content":""}))?);
            }
            "response.output_item.added" => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .filter(|v| *v < 1024)
                    .ok_or("invalid output index")?;
                if self.items.contains_key(&index) {
                    return Err("duplicate output item".into());
                }
                let item = event.get("item").ok_or("output item missing")?;
                let state = match item.get("type").and_then(Value::as_str) {
                    Some("message")
                        if item.get("role").and_then(Value::as_str) == Some("assistant") =>
                    {
                        Item::Message {
                            id: upstream_string(item.get("id"))?.into(),
                            parts: BTreeMap::new(),
                        }
                    }
                    Some("function_call") => {
                        let call = function(item)?;
                        let call_id = upstream_string(call.get("id"))?.to_owned();
                        let call_index = self.calls.len();
                        if !self.calls.insert(call_id) {
                            return Err("duplicate function call ID".into());
                        }
                        let mut arguments = Sent::default();
                        let text = upstream_string(call.pointer("/function/arguments"))?;
                        bounded_add(
                            &mut self.output_bytes,
                            text.len(),
                            crate::payload::PROJECTED_OUTPUT_BYTES,
                        )?;
                        arguments.append(text)?;
                        let mut delta = call.clone();
                        delta["index"] = json!(call_index);
                        chunks.push(self.chunk(json!({"tool_calls":[delta]}))?);
                        Item::Function {
                            id: upstream_string(item.get("id"))?.into(),
                            call,
                            index: call_index,
                            arguments,
                        }
                    }
                    Some("reasoning") => Item::Reasoning,
                    _ => return Err("unsupported upstream output item".into()),
                };
                self.items.insert(index, state);
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.function_call_arguments.delta" => {
                let kind = event["type"].as_str().expect("matched type");
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .ok_or("invalid output index")?;
                let item_id = upstream_string(event.get("item_id"))?;
                let delta = upstream_string(event.get("delta"))?;
                bounded_add(
                    &mut self.output_bytes,
                    delta.len(),
                    crate::payload::PROJECTED_OUTPUT_BYTES,
                )?;
                let value = match self
                    .items
                    .get_mut(&index)
                    .ok_or("delta has no output item")?
                {
                    Item::Function {
                        id,
                        index,
                        arguments,
                        ..
                    } if kind == "response.function_call_arguments.delta" && id == item_id => {
                        arguments.append(delta)?;
                        json!({"tool_calls":[{"index":index,"function":{"arguments":delta}}]})
                    }
                    Item::Message { id, parts }
                        if id == item_id && kind != "response.function_call_arguments.delta" =>
                    {
                        let index = event
                            .get("content_index")
                            .and_then(Value::as_u64)
                            .filter(|v| *v < 1024)
                            .ok_or("invalid content index")?;
                        let field = if kind == "response.refusal.delta" {
                            "refusal"
                        } else {
                            "content"
                        };
                        if !parts.contains_key(&index) {
                            bounded_add(&mut self.content_parts, 1, 1024)?;
                        }
                        let (existing, sent) = parts
                            .entry(index)
                            .or_insert_with(|| (field.into(), Sent::default()));
                        if existing != field {
                            return Err("content part type changed".into());
                        }
                        sent.append(delta)?;
                        json!({field:delta})
                    }
                    _ => return Err("delta output item mismatch".into()),
                };
                chunks.push(self.chunk(value)?);
            }
            "response.completed" | "response.incomplete" => {
                let response = event.get("response").ok_or("response missing")?;
                // Validate the whole terminal object before emitting a successful
                // finish. Only compact hashes of sent text/arguments are retained.
                let result = completion(response)?;
                let output = response["output"].as_array().expect("validated output");
                if output.len() != self.items.len() {
                    return Err("terminal output item count changed".into());
                }
                for (index, item) in output.iter().enumerate() {
                    let mut deltas = Vec::new();
                    match self
                        .items
                        .get_mut(&(index as u64))
                        .ok_or("terminal output item missing")?
                    {
                        Item::Function {
                            id,
                            call,
                            index,
                            arguments,
                        } => {
                            let completed = function(item)?;
                            if item.get("id").and_then(Value::as_str) != Some(id.as_str())
                                || completed["id"] != call["id"]
                                || completed["function"]["name"] != call["function"]["name"]
                            {
                                return Err("terminal function identity changed".into());
                            }
                            if let Some(text) = arguments.reconcile(upstream_string(
                                completed.pointer("/function/arguments"),
                            )?)? {
                                bounded_add(
                                    &mut self.output_bytes,
                                    text.len(),
                                    crate::payload::PROJECTED_OUTPUT_BYTES,
                                )?;
                                if !text.is_empty() {
                                    deltas.push(json!({"tool_calls":[{"index":index,"function":{"arguments":text}}]}));
                                }
                            }
                        }
                        Item::Message { id, parts } => {
                            if item.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                                return Err("terminal message identity changed".into());
                            }
                            let content = item
                                .get("content")
                                .and_then(Value::as_array)
                                .ok_or("terminal content missing")?;
                            if content.len() > 1024
                                || parts.keys().any(|index| *index >= content.len() as u64)
                            {
                                return Err("terminal content index changed".into());
                            }
                            for (index, part) in content.iter().enumerate() {
                                let (field, text) = match part.get("type").and_then(Value::as_str) {
                                    Some("output_text") => {
                                        ("content", upstream_string(part.get("text"))?)
                                    }
                                    Some("refusal") => {
                                        ("refusal", upstream_string(part.get("refusal"))?)
                                    }
                                    _ => return Err("unsupported terminal content".into()),
                                };
                                if !parts.contains_key(&(index as u64)) {
                                    bounded_add(&mut self.content_parts, 1, 1024)?;
                                }
                                let (existing, sent) = parts
                                    .entry(index as u64)
                                    .or_insert_with(|| (field.into(), Sent::default()));
                                if existing != field {
                                    return Err("terminal content type changed".into());
                                }
                                if let Some(text) = sent.reconcile(text)? {
                                    bounded_add(
                                        &mut self.output_bytes,
                                        text.len(),
                                        crate::payload::PROJECTED_OUTPUT_BYTES,
                                    )?;
                                    if !text.is_empty() {
                                        deltas.push(json!({field:text}));
                                    }
                                }
                            }
                        }
                        Item::Reasoning if item["type"] == "reasoning" => {}
                        _ => return Err("terminal output type changed".into()),
                    }
                    for delta in deltas {
                        chunks.push(self.chunk(delta)?);
                    }
                }
                let meta = self.meta.as_ref().ok_or("response.created missing")?;
                if result["id"] != meta.id
                    || result["model"] != meta.model
                    || result["created"] != meta.created
                {
                    return Err("response metadata changed".into());
                }
                chunks.push(data(&meta.chunk(
                    json!({}),
                    result["choices"][0]["finish_reason"].clone(),
                    self.include_usage,
                )));
                if self.include_usage {
                    chunks.push(data(&json!({"id":meta.id,"object":"chat.completion.chunk","created":meta.created,"model":meta.model,"choices":[],"usage":result["usage"]})));
                }
                chunks.push(b"data: [DONE]\n\n".to_vec());
            }
            "response.failed" | "error" => return Err("upstream response failed".into()),
            _ => {}
        }
        Ok(chunks)
    }
}

fn data(value: &Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

pub(crate) fn stream_error() -> Vec<u8> {
    data(
        &json!({"error":{"type":"upstream_error","code":"upstream_interrupted","message":"stream failed or cannot be represented; request outcome may be unknown"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_images_preserve_order_detail_and_text_boundaries() {
        let request = prepare(json!({"model":"gpt-test","messages":[
            {"role":"user","content":[
                {"type":"text","text":"first"},
                {"type":"image_url","image_url":{"url":"https://example.com/a.png","detail":"original"}},
                {"type":"text","text":""},
                {"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}},
                {"type":"text","text":"last"}
            ]}
        ]})).unwrap();
        assert_eq!(
            request.payload["input"][0]["content"],
            json!([
                {"type":"input_text","text":"first"},
                {"type":"input_image","image_url":"https://example.com/a.png","detail":"original"},
                {"type":"input_text","text":""},
                {"type":"input_image","image_url":"data:image/png;base64,AA=="},
                {"type":"input_text","text":"last"}
            ])
        );
        for detail in ["auto", "low", "high", "original"] {
            let request = prepare(json!({"model":"gpt-test","messages":[{"role":"user","content":[
                {"type":"image_url","image_url":{"url":"https://example.com/a.png","detail":detail}}
            ]}]})).unwrap();
            assert_eq!(request.payload["input"][0]["content"][0]["detail"], detail);
        }
    }

    #[test]
    fn invalid_image_forms_have_precise_redacted_errors() {
        for (image, field) in [
            (json!("https://example.com/private-marker"), "image_url"),
            (json!({}), "image_url.url"),
            (json!({"url":""}), "image_url.url"),
            (json!({"url":42}), "image_url.url"),
            (json!({"url":"private-marker\n"}), "image_url.url"),
            (
                json!({"url":"private-marker","detail":"private-marker"}),
                "image_url.detail",
            ),
            (
                json!({"url":"private-marker","detail":null}),
                "image_url.detail",
            ),
            (
                json!({"url":"private-marker","extra":true}),
                "image_url.extra",
            ),
        ] {
            let error = prepare(
                json!({"model":"gpt-test","messages":[{"role":"user","content":[
                    {"type":"image_url","image_url":image}
                ]}]}),
            )
            .err()
            .unwrap();
            assert_eq!(error.param, format!("messages[0].content[0].{field}"));
            assert!(!error.message.contains("private-marker"));
        }
        for role in ["assistant", "system", "developer"] {
            let error = prepare(
                json!({"model":"gpt-test","messages":[{"role":role,"content":[
                    {"type":"image_url","image_url":{"url":"private-marker"}}
                ]}]}),
            )
            .err()
            .unwrap();
            assert_eq!(error.param, "messages[0].content[0].type");
        }
        let error = prepare(json!({"model":"gpt-test","messages":[
            {"role":"assistant","tool_calls":[{"id":"call_1","type":"function","function":{"name":"f","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"call_1","content":[{"type":"image_url","image_url":{"url":"private-marker"}}]}
        ]})).err().unwrap();
        assert_eq!(error.param, "messages[1].content[0].type");
    }

    fn messages() -> Stream {
        let mut stream = Stream::new(false);
        stream.event(&json!({"type":"response.created","response":{"id":"r","model":"gpt-test","created_at":1}})).unwrap();
        for index in 0..2 {
            stream.event(&json!({"type":"response.output_item.added","output_index":index,"item":{"type":"message","role":"assistant","id":format!("m{index}")}})).unwrap();
        }
        stream
    }

    fn delta(output: usize, part: usize, text: &str) -> Value {
        json!({"type":"response.output_text.delta","output_index":output,"item_id":format!("m{output}"),"content_index":part,"delta":text})
    }

    #[test]
    fn stream_limits_apply_across_output_items() {
        // Each individual part is within its limit, but their aggregate is not.
        let mut stream = messages();
        let text = "a".repeat(524_288);
        stream.event(&delta(0, 0, &text)).unwrap();
        stream.event(&delta(1, 0, &text)).unwrap();
        assert!(stream.event(&delta(1, 1, "a")).is_err());

        // Empty parts cannot bypass the shared bound on retained metadata.
        let mut stream = messages();
        for output in 0..2 {
            for part in 0..512 {
                stream.event(&delta(output, part, "")).unwrap();
            }
        }
        assert!(stream.event(&delta(1, 512, "")).is_err());
    }
}
