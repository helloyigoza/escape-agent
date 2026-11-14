//! JSON-RPC payload framing helpers shared by the daemon and its pool.

use serde_json::Value;

pub(super) fn extract_mcp_text(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return result.as_str().map(ToString::to_string).unwrap_or_default();
    };
    let mut parts = Vec::new();
    for item in content {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            parts.push(text);
        }
    }
    parts.join("\n")
}

pub(super) fn truncate_snippet(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
