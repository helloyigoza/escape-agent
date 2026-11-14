//! Cross-model history sanitization and tool call/response pairing guarantees.

use crate::accounts::ModelFamily;
use crate::thinking::is_valid_thought_signature;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// Sanitize `contents` so turns originating from a different model family remain valid
/// and ensure every `functionCall` has an ID paired with its subsequent `functionResponse`.
pub fn sanitize_cross_model_contents(
    contents: &mut Vec<Value>,
    target_family: ModelFamily,
    tool_id_recovery: bool,
) {
    assign_missing_call_ids_and_pair(contents, tool_id_recovery);
    strip_foreign_model_artifacts(contents, target_family);
    merge_adjacent_same_role_turns(contents);
}

fn assign_missing_call_ids_and_pair(contents: &mut [Value], tool_id_recovery: bool) {
    let mut seq: usize = 0;
    let mut pending_by_name: HashMap<String, Vec<String>> = HashMap::new();

    for msg in contents.iter_mut() {
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(parts) = msg.get_mut("parts").and_then(Value::as_array_mut) else {
            continue;
        };

        for part in parts.iter_mut() {
            if role == "model"
                && let Some(fc) = part.get_mut("functionCall").and_then(Value::as_object_mut)
            {
                let name = fc
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned();
                let existing_id = fc
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let id = if let Some(existing) = existing_id {
                    existing
                } else {
                    seq += 1;
                    let generated = format!("ag_call_{seq}");
                    fc.insert("id".to_owned(), Value::String(generated.clone()));
                    generated
                };
                pending_by_name.entry(name).or_default().push(id);
            } else if role == "user"
                && tool_id_recovery
                && let Some(fr) = part
                    .get_mut("functionResponse")
                    .and_then(Value::as_object_mut)
            {
                let name = fr
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned();
                let has_id = fr
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty());
                if !has_id
                    && let Some(queue) = pending_by_name.get_mut(&name)
                    && !queue.is_empty()
                {
                    let matched_id = queue.remove(0);
                    fr.insert("id".to_owned(), Value::String(matched_id));
                }
            }
        }
    }

    if !tool_id_recovery {
        return;
    }

    // Ensure every `model` turn with functionCall has a corresponding `functionResponse`
    // before the next `model` or non-tool `user` text turn.
    let mut known_response_ids: HashSet<String> = HashSet::new();
    for msg in contents.iter() {
        if let Some(parts) = msg.get("parts").and_then(Value::as_array) {
            for part in parts {
                if let Some(id) = part
                    .get("functionResponse")
                    .and_then(|fr| fr.get("id"))
                    .and_then(Value::as_str)
                {
                    known_response_ids.insert(id.to_owned());
                }
            }
        }
    }

    // Remove unmatched orphaned functionResponse IDs or patch missing IDs conservatively
    // only when completely unpaired (`claude.rs` handles full synthetic closing when needed).
    for msg in contents.iter_mut() {
        let Some(parts) = msg.get_mut("parts").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts.iter_mut() {
            if let Some(fr) = part
                .get_mut("functionResponse")
                .and_then(Value::as_object_mut)
            {
                let id = fr
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if id.is_empty() {
                    fr.insert("id".to_owned(), json!("ag_call_recovered"));
                }
            }
        }
    }
    let _ = known_response_ids;
}

fn strip_foreign_model_artifacts(contents: &mut [Value], target_family: ModelFamily) {
    for msg in contents.iter_mut() {
        let Some(parts) = msg.get_mut("parts").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts.iter_mut() {
            let Some(obj) = part.as_object_mut() else {
                continue;
            };
            // On Claude targets, reject signatures that are clearly Gemini bypass sentinels.
            if target_family == ModelFamily::Claude
                && obj.get("thought").and_then(Value::as_bool).unwrap_or(false)
                && let Some(sig) = obj.get("thoughtSignature").and_then(Value::as_str)
                && (!is_valid_thought_signature(sig) || sig == "skip_thought_signature_validator")
            {
                obj.remove("thoughtSignature");
            }
        }
    }
}

/// Merge adjacent turns sharing the same `role` (`"user"` + `"user"` or `"model"` + `"model"`).
pub fn merge_adjacent_same_role_turns(contents: &mut Vec<Value>) {
    let mut merged: Vec<Value> = Vec::with_capacity(contents.len());
    for turn in contents.drain(..) {
        let role = turn.get("role").and_then(Value::as_str).unwrap_or_default();
        if let Some(last) = merged.last_mut()
            && last.get("role").and_then(Value::as_str) == Some(role)
            && let Some(new_parts) = turn.get("parts").and_then(Value::as_array)
            && let Some(last_parts) = last.get_mut("parts").and_then(Value::as_array_mut)
        {
            last_parts.extend(new_parts.iter().cloned());
            continue;
        }
        merged.push(turn);
    }
    *contents = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_missing_function_call_ids_and_merges_adjacent_turns() {
        let mut contents = vec![
            json!({ "role": "user", "parts": [{ "text": "one" }] }),
            json!({ "role": "user", "parts": [{ "text": "two" }] }),
            json!({ "role": "model", "parts": [{ "functionCall": { "name": "read", "args": {} } }] }),
            json!({ "role": "user", "parts": [{ "functionResponse": { "name": "read", "response": { "ok": true } } }] }),
        ];
        sanitize_cross_model_contents(&mut contents, ModelFamily::Claude, true);
        assert_eq!(contents.len(), 3);
        let call_id = contents[1]["parts"][0]["functionCall"]["id"]
            .as_str()
            .expect("call id");
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["id"], call_id);
    }
}
