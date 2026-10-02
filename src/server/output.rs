//! Assemble JSON output from Codex's completed items when the final event omits
//! them. The accumulator is request-local, bounded and never persisted.
use crate::Result;
use serde_json::Value;
use std::collections::BTreeMap;

/// Project the current Codex Responses compaction protocol onto the public
/// compact endpoint. Never issue another inference when projection fails.
pub(super) fn compaction(response: &Value) -> Result<Value> {
    if response["status"] != "completed" {
        return Err("compaction did not complete".into());
    }
    let output = response["output"]
        .as_array()
        .ok_or("compaction output missing")?;
    if output.len() != 1
        || output[0]["type"] != "compaction"
        || !output[0]["encrypted_content"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    {
        return Err("compaction requires exactly one opaque checkpoint".into());
    }
    let mut value = serde_json::json!({"object":"response.compaction","output":output,"usage":response["usage"]});
    for name in ["id", "created_at"] {
        if let Some(field) = response.get(name) {
            value[name] = field.clone();
        }
    }
    Ok(value)
}

#[derive(Default)]
pub(super) struct CompletionOutput {
    items: BTreeMap<u64, Value>,
    bytes: usize,
}
impl CompletionOutput {
    pub fn observe(&mut self, event: &mut Value) -> Result<()> {
        match event["type"].as_str() {
            Some("response.output_item.done") => {
                let index = event["output_index"]
                    .as_u64()
                    .filter(|index| *index < 1024)
                    .ok_or("invalid completed output index")?;
                let item = event
                    .get("item")
                    .filter(|item| item.is_object())
                    .ok_or("completed output item missing")?;
                if let Some(old) = self.items.get(&index) {
                    if old != item {
                        return Err("completed output item changed".into());
                    }
                    return Ok(());
                }
                let bytes = serde_json::to_vec(item)?.len() + 1;
                if self.bytes.saturating_add(bytes) > crate::payload::PROJECTED_OUTPUT_BYTES {
                    return Err("assembled output exceeds 1 MiB".into());
                }
                self.bytes += bytes;
                self.items.insert(index, item.clone());
            }
            Some("response.completed" | "response.incomplete") => {
                let response = event
                    .get_mut("response")
                    .and_then(Value::as_object_mut)
                    .ok_or("terminal response missing")?;
                match response.get("output") {
                    Some(Value::Array(items)) if !items.is_empty() => return Ok(()),
                    None | Some(Value::Array(_)) => {}
                    _ => return Err("invalid terminal output".into()),
                }
                if self.items.keys().copied().ne(0..self.items.len() as u64) {
                    return Err("completed output items are missing".into());
                }
                response.insert(
                    "output".into(),
                    Value::Array(std::mem::take(&mut self.items).into_values().collect()),
                );
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn ordered_done_items_fill_empty_terminal_without_overwriting_provided_output() {
        let mut output = CompletionOutput::default();
        for index in [1, 0, 0] {
            output.observe(&mut json!({"type":"response.output_item.done","output_index":index,"item":{"id":index,"type":"reasoning"}})).unwrap();
        }
        let mut provided =
            json!({"type":"response.completed","response":{"output":[{"id":"provided"}]}});
        output.observe(&mut provided).unwrap();
        assert_eq!(provided["response"]["output"][0]["id"], "provided");
        let mut terminal = json!({"type":"response.completed","response":{"output":[]}});
        output.observe(&mut terminal).unwrap();
        assert_eq!(terminal["response"]["output"][0]["id"], 0);
        assert_eq!(terminal["response"]["output"][1]["id"], 1);
    }
    #[test]
    fn conflicting_missing_and_oversized_items_cannot_create_a_successful_completion() {
        let mut output = CompletionOutput::default();
        output.observe(&mut json!({"type":"response.output_item.done","output_index":1,"item":{"id":"one"}})).unwrap();
        assert!(output
            .observe(&mut json!({"type":"response.completed","response":{}}))
            .is_err());
        assert!(output.observe(&mut json!({"type":"response.output_item.done","output_index":1,"item":{"id":"changed"}})).is_err());
        assert!(output
            .observe(&mut json!({"type":"response.output_item.done","output_index":1024,"item":{}}))
            .is_err());
        let mut output = CompletionOutput::default();
        for index in 0..2 {
            output.observe(&mut json!({"type":"response.output_item.done","output_index":index,"item":{"text":"x".repeat(400_000)}})).unwrap();
        }
        assert!(output.observe(&mut json!({"type":"response.output_item.done","output_index":2,"item":{"text":"x".repeat(400_000)}})).is_err());
    }
}
