//! Transient recovery of the latest completed WS window. Never stored or logged.
use serde_json::Value;
use std::{io, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) const TOTAL_BYTES: usize = 64 * 1024 * 1024;
const WINDOW_BYTES: usize = crate::payload::INFERENCE_REQUEST_BYTES;
const ITEMS: usize = 16384;

pub(super) struct Window {
    response: String,
    input: Vec<Value>,
    _bytes: OwnedSemaphorePermit,
}

// Count without allocating another serialized history or accepting expansion.
struct Size(usize);
impl io::Write for Size {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        if self.0 > WINDOW_BYTES {
            return Err(io::Error::other("continuation window too large"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Window {
    pub(super) fn expand(&self, payload: &mut Value) -> bool {
        if payload
            .get("previous_response_id")
            .is_none_or(Value::is_null)
        {
            return true;
        }
        if payload["previous_response_id"].as_str() != Some(&self.response) {
            return false;
        }
        let Some(delta) = payload["input"].as_array() else {
            return false;
        };
        if self.input.len().saturating_add(delta.len()) > ITEMS {
            return false;
        }
        let mut size = Size(0);
        if serde_json::to_writer(&mut size, &self.input).is_err()
            || serde_json::to_writer(&mut size, delta).is_err()
        {
            return false;
        }
        let mut input = self.input.clone();
        input.extend(delta.iter().cloned());
        let body = payload.as_object_mut().expect("validated request");
        let original_input = body.insert("input".into(), Value::Array(input));
        let previous = body.remove("previous_response_id");
        if Self::fits(payload) {
            return true;
        }
        // Failed soft-threshold recovery must leave the original request usable.
        let body = payload.as_object_mut().expect("validated request");
        if let Some(input) = original_input {
            body.insert("input".into(), input);
        }
        if let Some(previous) = previous {
            body.insert("previous_response_id".into(), previous);
        }
        false
    }

    pub(super) fn fits(payload: &Value) -> bool {
        serde_json::to_writer(&mut Size(0), payload).is_ok()
    }

    pub(super) fn completed(
        old: Option<Self>,
        payload: &mut Value,
        response: &mut Value,
        budget: &Arc<Semaphore>,
    ) -> Option<Self> {
        let id = response["id"].as_str()?.to_owned();
        let input = payload.get_mut("input")?.take();
        let mut input = match input {
            Value::Array(items) => items,
            Value::String(text) => vec![serde_json::json!({"role":"user","content":text})],
            _ => return None,
        };
        let mut combined = if let Some(previous) = payload["previous_response_id"].as_str() {
            let old = old?;
            if old.response != previous {
                return None;
            }
            old.input
        } else {
            drop(old);
            Vec::new()
        };
        let output = response.get_mut("output")?.take();
        let Value::Array(mut output) = output else {
            return None;
        };
        if combined
            .len()
            .saturating_add(input.len())
            .saturating_add(output.len())
            > ITEMS
        {
            return None;
        }
        combined.append(&mut input);
        combined.append(&mut output);
        let mut size = Size(0);
        serde_json::to_writer(&mut size, &combined).ok()?;
        // Old reservation is released before reserving the new complete window.
        let permit = budget
            .clone()
            .try_acquire_many_owned(size.0.max(1) as u32)
            .ok()?;
        Some(Self {
            response: id,
            input: combined,
            _bytes: permit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn oversized_expansion_leaves_original_incremental_request_unchanged() {
        let budget = Arc::new(Semaphore::new(WINDOW_BYTES));
        let mut request = json!({"input":[{"role":"user","content":"synthetic"}]});
        let mut response = json!({"id":"a","output":[]});
        let window = Window::completed(None, &mut request, &mut response, &budget).unwrap();
        let mut delta =
            json!({"previous_response_id":"a","input":[],"instructions":"x".repeat(WINDOW_BYTES)});
        let before = delta.clone();
        assert!(!window.expand(&mut delta));
        assert_eq!(delta, before);
    }
    #[test]
    fn latest_delta_recovers_complete_tool_history_and_releases_budget() {
        let budget = Arc::new(Semaphore::new(4096));
        let mut request = json!({"input":[{"role":"user","content":"remember"}]});
        let mut response =
            json!({"id":"a","output":[{"type":"function_call","call_id":"tool","arguments":"{}"}]});
        let window = Window::completed(None, &mut request, &mut response, &budget).unwrap();
        let mut delta = json!({"previous_response_id":"a","input":[{"type":"function_call_output","call_id":"tool","output":"done"}]});
        assert!(window.expand(&mut delta));
        assert!(delta.get("previous_response_id").is_none());
        assert_eq!(delta["input"].as_array().unwrap().len(), 3);
        assert_eq!(delta["input"][1]["call_id"], delta["input"][2]["call_id"]);
        assert!(!window.expand(&mut json!({"previous_response_id":"older","input":[]})));
        drop(window);
        assert_eq!(budget.available_permits(), 4096);
        assert!(Window::completed(
            None,
            &mut json!({"input":[{"content":"x".repeat(4096)}]}),
            &mut json!({"id":"b","output":[]}),
            &budget
        )
        .is_none());
        assert_eq!(budget.available_permits(), 4096);
    }
}
