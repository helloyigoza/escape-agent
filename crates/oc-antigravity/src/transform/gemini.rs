//! Gemini-over-Antigravity / Gemini CLI request schema cleaning and thinking configuration.

use crate::schema::to_gemini_schema;
use crate::transform::model_resolver::ThinkingMode;
use serde_json::{Map, Value, json};

/// Apply Gemini-specific schema conversion and thinking settings to a Cloud Code `request` object.
pub fn transform_gemini_request(request_obj: &mut Map<String, Value>, thinking: &ThinkingMode) {
    if let Some(tools) = request_obj.get_mut("tools").and_then(Value::as_array_mut) {
        for tool_entry in tools.iter_mut() {
            if let Some(decls) = tool_entry
                .get_mut("functionDeclarations")
                .and_then(Value::as_array_mut)
            {
                for decl in decls.iter_mut() {
                    if let Some(obj) = decl.as_object_mut() {
                        let raw = obj
                            .get("parameters")
                            .or_else(|| obj.get("parametersJsonSchema"))
                            .cloned();
                        if let Some(schema) = raw {
                            obj.remove("parametersJsonSchema");
                            obj.insert("parameters".to_owned(), to_gemini_schema(&schema));
                        }
                    }
                }
            }
        }
    }

    let gen_cfg = request_obj
        .entry("generationConfig".to_owned())
        .or_insert_with(|| json!({}));
    let Some(gen_obj) = gen_cfg.as_object_mut() else {
        return;
    };

    match thinking {
        ThinkingMode::GeminiLevel(level) => {
            gen_obj.insert(
                "thinkingConfig".to_owned(),
                json!({
                    "thinkingLevel": level,
                    "includeThoughts": true
                }),
            );
        }
        ThinkingMode::Budget(budget) if *budget > 0 => {
            gen_obj.insert(
                "thinkingConfig".to_owned(),
                json!({
                    "thinkingBudget": *budget,
                    "includeThoughts": true
                }),
            );
        }
        _ => {
            gen_obj.remove("thinkingConfig");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_tool_schemas_to_uppercase_gemini_and_sets_thinking_level() {
        let mut req = json!({
            "tools": [{
                "functionDeclarations": [{
                    "name": "read",
                    "description": "Read file",
                    "parametersJsonSchema": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }
                }]
            }]
        });
        let obj = req.as_object_mut().expect("obj");
        transform_gemini_request(obj, &ThinkingMode::GeminiLevel("high".to_owned()));
        assert_eq!(
            obj["tools"][0]["functionDeclarations"][0]["parameters"]["type"],
            "OBJECT"
        );
        assert!(
            obj["tools"][0]["functionDeclarations"][0]
                .get("parametersJsonSchema")
                .is_none()
        );
        assert_eq!(
            obj["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
    }
}
