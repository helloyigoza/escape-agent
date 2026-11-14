//! Conversation thinking-block inspection, corruption recovery, tool-loop repair,
//! and thought-signature sanitization for Antigravity Cloud Code requests.

use crate::accounts::ModelFamily;
use crate::constants::SKIP_THOUGHT_SIGNATURE;
use crate::signature_cache::SignatureCache;
use serde_json::{Value, json};

/// Summary of thinking state across Cloud Code `contents`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationThinkingState {
    /// True if the last turn is a `model` turn containing `functionCall` without valid signed thinking.
    pub in_broken_tool_loop: bool,
    /// Number of unsigned or malformed thinking parts stripped/fixed.
    pub unsigned_thinking_parts: usize,
    /// Number of assistant tool-use turns lacking a leading thinking block.
    pub missing_thinking_tool_turns: usize,
}

/// Check whether a signature string is minimally plausible for Cloud Code validation.
#[must_use]
pub fn is_valid_thought_signature(sig: &str) -> bool {
    let trimmed = sig.trim();
    trimmed == SKIP_THOUGHT_SIGNATURE || trimmed.len() >= 16
}

/// Inspect serialized Cloud Code `contents` for thinking/tool ordering issues.
#[must_use]
pub fn analyze_conversation_state(
    contents: &[Value],
    family: ModelFamily,
) -> ConversationThinkingState {
    let mut state = ConversationThinkingState::default();

    for (idx, msg) in contents.iter().enumerate() {
        let Some(role) = msg.get("role").and_then(Value::as_str) else {
            continue;
        };
        if role != "model" {
            continue;
        }
        let Some(parts) = msg.get("parts").and_then(Value::as_array) else {
            continue;
        };

        let mut has_function_call = false;
        let mut has_valid_leading_thinking = false;
        let mut first_non_empty_checked = false;

        for part in parts {
            if part.get("functionCall").is_some() {
                has_function_call = true;
            }
            let is_thought = part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let sig = part.get("thoughtSignature").and_then(Value::as_str);

            if is_thought {
                let sig_ok = sig.is_some_and(is_valid_thought_signature);
                if !sig_ok {
                    state.unsigned_thinking_parts += 1;
                }
                if !first_non_empty_checked {
                    has_valid_leading_thinking = sig_ok || family == ModelFamily::Gemini;
                    first_non_empty_checked = true;
                }
            } else if !first_non_empty_checked {
                first_non_empty_checked = true;
            }
        }

        if has_function_call && !has_valid_leading_thinking {
            state.missing_thinking_tool_turns += 1;
            if idx + 1 == contents.len() {
                state.in_broken_tool_loop = true;
            }
        }
    }

    state
}

/// Determine whether a conversation requires thinking-block recovery before sending upstream.
#[must_use]
pub fn needs_thinking_recovery(contents: &[Value], family: ModelFamily) -> bool {
    let state = analyze_conversation_state(contents, family);
    state.in_broken_tool_loop
        || state.unsigned_thinking_parts > 0
        || (family == ModelFamily::Claude && state.missing_thinking_tool_turns > 0)
}

/// Recover signatures from [`SignatureCache`], remove invalid/unsigned thinking parts, and repair
/// broken tool-call/thinking sequences across Cloud Code `contents`.
#[allow(clippy::too_many_arguments)]
pub fn repair_contents_thinking(
    contents: &mut Vec<Value>,
    family: ModelFamily,
    thinking_enabled: bool,
    keep_thinking: bool,
    session_key: &str,
    cache: Option<&mut SignatureCache>,
    now_ms: i64,
) {
    let mut cache = cache;

    for msg in contents.iter_mut() {
        let Some(role) = msg.get("role").and_then(Value::as_str) else {
            continue;
        };
        if role != "model" {
            continue;
        }
        let Some(parts) = msg.get_mut("parts").and_then(Value::as_array_mut) else {
            continue;
        };

        let mut cleaned = Vec::with_capacity(parts.len());
        for mut part in parts.drain(..) {
            let is_thought = part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if is_thought {
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                let mut sig = part
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                    .filter(|s| is_valid_thought_signature(s))
                    .map(str::to_owned);

                // Attempt cache recovery when signature is missing/invalid.
                if sig.is_none()
                    && !text.is_empty()
                    && let Some(ref mut c) = cache
                    && let Some(hit) = c.get(session_key, &text, now_ms)
                    && (family == ModelFamily::Gemini || hit.family == ModelFamily::Claude)
                {
                    sig = Some(hit.signature);
                }

                if family == ModelFamily::Claude {
                    if let Some(valid_sig) = sig {
                        if let Some(obj) = part.as_object_mut() {
                            obj.insert("thoughtSignature".to_owned(), Value::String(valid_sig));
                        }
                        if thinking_enabled || keep_thinking {
                            cleaned.push(part);
                        }
                    }
                    // Drop unsigned thinking parts on Claude; Cloud Code rejects unsigned blocks.
                    continue;
                }

                // Gemini family accepts sentinel when signature is unrecoverable, or drop if disabled.
                if sig.is_none() {
                    if !thinking_enabled && !keep_thinking {
                        continue;
                    }
                    if let Some(obj) = part.as_object_mut() {
                        obj.insert(
                            "thoughtSignature".to_owned(),
                            Value::String(SKIP_THOUGHT_SIGNATURE.to_owned()),
                        );
                    }
                } else if let Some(valid_sig) = sig
                    && let Some(obj) = part.as_object_mut()
                {
                    obj.insert("thoughtSignature".to_owned(), Value::String(valid_sig));
                }
                cleaned.push(part);
            } else {
                // On Gemini 3+, functionCall parts may also require thoughtSignature when thinking is active.
                if family == ModelFamily::Gemini
                    && thinking_enabled
                    && part.get("functionCall").is_some()
                {
                    let has_sig = part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .is_some_and(is_valid_thought_signature);
                    if !has_sig && let Some(obj) = part.as_object_mut() {
                        obj.insert(
                            "thoughtSignature".to_owned(),
                            Value::String(SKIP_THOUGHT_SIGNATURE.to_owned()),
                        );
                    }
                }
                cleaned.push(part);
            }
        }

        // Reorder any preserved thinking parts to appear first in model turns (Claude ordering rule).
        if family == ModelFamily::Claude {
            let (thoughts, others): (Vec<Value>, Vec<Value>) = cleaned
                .into_iter()
                .partition(|p| p.get("thought").and_then(Value::as_bool).unwrap_or(false));
            *parts = thoughts.into_iter().chain(others).collect();
        } else {
            *parts = cleaned;
        }
    }

    // Drop completely empty model messages left after stripping unsigned thinking.
    contents.retain(|msg| {
        msg.get("parts")
            .and_then(Value::as_array)
            .is_none_or(|p| !p.is_empty())
    });
}

/// Close a broken tool-use loop before injecting a synthetic user follow-up so Claude accepts
/// conversations interrupted after a tool call without valid thinking blocks.
pub fn close_tool_loop_for_thinking(contents: &mut Vec<Value>) {
    let needs_closing = contents.last().is_some_and(|msg| {
        msg.get("role").and_then(Value::as_str) == Some("model")
            && msg
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| parts.iter().any(|p| p.get("functionCall").is_some()))
    });

    if !needs_closing {
        return;
    }

    let mut responses = Vec::new();
    if let Some(parts) = contents
        .last()
        .and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(fc) = part.get("functionCall")
                && let Some(name) = fc.get("name").and_then(Value::as_str)
            {
                let mut fr = serde_json::Map::new();
                fr.insert("name".to_owned(), json!(name));
                fr.insert(
                    "response".to_owned(),
                    json!({ "error": "Tool execution interrupted before completion." }),
                );
                if let Some(id) = fc.get("id").and_then(Value::as_str) {
                    fr.insert("id".to_owned(), json!(id));
                }
                responses.push(json!({ "functionResponse": fr }));
            }
        }
    }

    if !responses.is_empty() {
        contents.push(json!({
            "role": "user",
            "parts": responses,
        }));
    }
    contents.push(json!({
        "role": "model",
        "parts": [{ "text": "[Previous thinking/tool turn recovered]" }],
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_drops_unsigned_thinking_and_moves_signed_first() {
        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                { "text": "calling tool", "functionCall": { "name": "read", "args": {} } },
                { "thought": true, "text": "unsigned" },
                { "thought": true, "text": "signed", "thoughtSignature": "abcdef0123456789abcdef" }
            ]
        })];

        repair_contents_thinking(
            &mut contents,
            ModelFamily::Claude,
            true,
            false,
            "ses",
            None,
            1000,
        );
        let parts = contents[0]["parts"].as_array().expect("parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["thought"], true);
        assert_eq!(parts[0]["text"], "signed");
        assert!(parts[1].get("functionCall").is_some());
    }

    #[test]
    fn gemini_injects_skip_thought_signature_when_missing_on_function_call() {
        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                { "functionCall": { "name": "read", "args": {} } }
            ]
        })];
        repair_contents_thinking(
            &mut contents,
            ModelFamily::Gemini,
            true,
            false,
            "ses",
            None,
            1000,
        );
        assert_eq!(
            contents[0]["parts"][0]["thoughtSignature"],
            SKIP_THOUGHT_SIGNATURE
        );
    }
}
