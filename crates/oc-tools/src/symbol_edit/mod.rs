//! `symbol_edit` tool: symbol-level atomic edits (`replace`, `insert_before`,
//! `insert_after`, `preview`) backed by `ripwire --mcp` (`replace_symbol_body`,
//! `insert_before_symbol`, `insert_after_symbol`, `edit_check`) with a cached
//! pure-Rust AST-span fallback, BOM/EOL preservation, and unified diff receipts.

mod file;
mod native;
mod resolve;
mod ripwire;

use async_trait::async_trait;
use serde_json::{Value, json};

use self::native::execute_native_symbol_edit;
use self::ripwire::try_ripwire_symbol_edit;

#[cfg(test)]
use self::resolve::split_symbol_and_file;

use crate::code_map::tree_root_for;
use crate::util;
use crate::{Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Default, Clone, Copy)]
pub struct SymbolEditTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolOp {
    Replace,
    InsertBefore,
    InsertAfter,
    Preview,
}

impl SymbolOp {
    fn parse(raw: &str) -> Result<Self, ToolError> {
        match raw.trim() {
            "replace" => Ok(Self::Replace),
            "insert_before" => Ok(Self::InsertBefore),
            "insert_after" => Ok(Self::InsertAfter),
            "preview" => Ok(Self::Preview),
            other => Err(ToolError::new(format!(
                "Invalid `operation` '{other}'. Expected 'replace', 'insert_before', 'insert_after', or 'preview'."
            ))),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::InsertBefore => "insert_before",
            Self::InsertAfter => "insert_after",
            Self::Preview => "preview",
        }
    }

    const fn mcp_tool(self) -> &'static str {
        match self {
            Self::Replace => "replace_symbol_body",
            Self::InsertBefore => "insert_before_symbol",
            Self::InsertAfter => "insert_after_symbol",
            Self::Preview => "edit_check",
        }
    }
}

#[async_trait]
impl Tool for SymbolEditTool {
    fn name(&self) -> &str {
        "symbol_edit"
    }

    fn description(&self) -> &str {
        "Atomically replace or insert code anchored to a structural symbol (`SYM`, `file:SYM`, \
         or `@FILE:LINE`), or preview contract impact before writing (`operation=\"preview\"`). \
         Eliminates fragile line-number/whitespace matching and returns unified diff + \
         caller/test checks in a single call. Uses `ripwire --mcp` when installed with a \
         pure-Rust AST-span fallback."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["replace", "insert_before", "insert_after", "preview"],
                    "description": "Edit mode: 'replace' replaces the full symbol definition, \
                                    'insert_before' inserts immediately before the symbol, \
                                    'insert_after' inserts immediately after the symbol, \
                                    'preview' checks contract impact/span without modifying files."
                },
                "symbol": {
                    "type": "string",
                    "description": "Target symbol: `name`, `file:name`, or `@file:line`."
                },
                "path": {
                    "type": "string",
                    "description": "Optional file or directory path to disambiguate `symbol`."
                },
                "content": {
                    "type": "string",
                    "description": "Replacement body or text to insert (required for replace/insert_before/insert_after; optional hypothetical body for preview)."
                },
                "post_check": {
                    "type": "boolean",
                    "description": "Run ripwire post-edit contract & impacted-test checks (default: true)."
                }
            },
            "required": ["operation", "symbol"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, input: Value) -> Result<ToolResult, ToolError> {
        let op = SymbolOp::parse(&util::required_string(&input, "operation")?)?;
        let symbol = util::required_string(&input, "symbol")?.trim().to_string();
        if symbol.is_empty() {
            return Err(ToolError::new("`symbol` must not be empty"));
        }
        let path_arg = util::optional_string(&input, "path")?
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        let content = util::optional_string(&input, "content")?;
        let post_check = util::optional_bool(&input, "post_check")?.unwrap_or(true);

        let body_text = match op {
            SymbolOp::Preview => content,
            SymbolOp::Replace => {
                let text = content.ok_or_else(|| {
                    ToolError::new("Parameter `content` is required for operation 'replace'")
                })?;
                if text.trim().is_empty() {
                    return Err(ToolError::new("`content` must not be empty for `replace`"));
                }
                Some(text)
            }
            SymbolOp::InsertBefore | SymbolOp::InsertAfter => {
                let text = content.ok_or_else(|| {
                    ToolError::new(format!(
                        "Parameter `content` is required for operation '{}'",
                        op.as_str()
                    ))
                })?;
                if text.is_empty() {
                    return Err(ToolError::new(format!(
                        "`content` must not be empty for '{}'",
                        op.as_str()
                    )));
                }
                Some(text)
            }
        };

        let scope_path = match path_arg.as_deref() {
            Some(p) => util::resolve(ctx, p),
            None => ctx.directory.clone(),
        };
        if path_arg.is_some() && !scope_path.exists() {
            return Err(ToolError::new(format!(
                "Path does not exist: {}",
                scope_path.display()
            )));
        }

        let root = tree_root_for(ctx, &scope_path);

        if let Some(result) = try_ripwire_symbol_edit(
            ctx,
            &root,
            &scope_path,
            op,
            &symbol,
            path_arg.as_deref(),
            body_text.as_deref(),
            post_check,
        )
        .await?
        {
            return Ok(result);
        }

        execute_native_symbol_edit(
            ctx,
            &root,
            &scope_path,
            op,
            &symbol,
            path_arg.as_deref(),
            body_text.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn native_replace_insert_before_insert_after_preserve_bom_and_crlf() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("sample.rs");
        let original = "\u{feff}fn alpha() {\r\n    let _s = \"}\";\r\n    let x = 1;\r\n}\r\n\r\nfn beta() -> i32 {\r\n    2\r\n}\r\n";
        fs::write(&src, original.as_bytes()).unwrap();

        let ctx = ToolContext {
            session_id: "s1".to_string(),
            directory: dir.path().to_path_buf(),
            cwd: dir.path().to_path_buf(),
            cancel: None,
        };

        // Test pure-Rust span engine directly so results are 100% deterministic
        // even when `ripwire` is installed on PATH.
        let prev = execute_native_symbol_edit(
            &ctx,
            dir.path(),
            &src,
            SymbolOp::Preview,
            "alpha",
            Some("sample.rs"),
            Some("fn alpha() {\n    let x = 99;\n}"),
        )
        .await
        .unwrap();
        assert!(prev.output.unwrap().contains("fn alpha()"));
        assert_eq!(fs::read_to_string(&src).unwrap(), original);

        // Replace `alpha` body (note the `}` inside the string literal must NOT end the span early!).
        let replaced = execute_native_symbol_edit(
            &ctx,
            dir.path(),
            &src,
            SymbolOp::Replace,
            "alpha",
            Some("sample.rs"),
            Some("fn alpha() {\n    let x = 99;\n}"),
        )
        .await
        .unwrap();
        let meta = replaced.metadata.unwrap();
        assert_eq!(meta["files"].as_array().unwrap().len(), 1);
        let after_replace = fs::read_to_string(&src).unwrap();
        assert!(after_replace.starts_with('\u{feff}'));
        assert!(after_replace.contains("\r\n"));
        assert!(!after_replace.contains("let x = 1;"));
        assert!(after_replace.contains("fn beta() -> i32 {\r\n    2\r\n}"));

        // Insert doc comment before `beta` and a helper after `beta`.
        execute_native_symbol_edit(
            &ctx,
            dir.path(),
            &src,
            SymbolOp::InsertBefore,
            "beta",
            Some("sample.rs"),
            Some("/// Doc above beta."),
        )
        .await
        .unwrap();
        execute_native_symbol_edit(
            &ctx,
            dir.path(),
            &src,
            SymbolOp::InsertAfter,
            "beta",
            Some("sample.rs"),
            Some("\nfn gamma() -> i32 {\n    3\n}"),
        )
        .await
        .unwrap();

        let final_disk = fs::read_to_string(&src).unwrap();
        assert!(final_disk.contains("/// Doc above beta.\r\nfn beta()"));
        assert!(final_disk.contains("fn gamma() -> i32 {\r\n    3\r\n}"));
        assert!(final_disk.starts_with('\u{feff}'));
    }

    #[tokio::test]
    async fn ambiguous_symbol_errors_without_editing() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/a.rs"),
            "pub fn helper() -> i32 { 1 }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/b.rs"),
            "pub fn helper() -> i32 { 2 }\n",
        )
        .unwrap();

        let ctx = ToolContext {
            session_id: "s1".to_string(),
            directory: dir.path().to_path_buf(),
            cwd: dir.path().to_path_buf(),
            cancel: None,
        };
        let err = execute_native_symbol_edit(
            &ctx,
            dir.path(),
            dir.path(),
            SymbolOp::Replace,
            "helper",
            None,
            Some("pub fn helper() -> i32 { 3 }"),
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("disambiguate") || err.message.contains("matched"));
    }

    #[test]
    fn split_symbol_and_file_handles_drive_letters_and_qualified_names() {
        assert_eq!(
            split_symbol_and_file("C:\\repo\\src\\lib.rs:alpha", None),
            (
                "alpha".to_string(),
                Some("C:\\repo\\src\\lib.rs".to_string())
            )
        );
        assert_eq!(
            split_symbol_and_file("src/lib.rs:alpha", None),
            ("alpha".to_string(), Some("src/lib.rs".to_string()))
        );
        assert_eq!(
            split_symbol_and_file("Type::method", None),
            ("Type::method".to_string(), None)
        );
        assert_eq!(
            split_symbol_and_file("C:\\repo\\src\\lib.rs:alpha", Some("C:\\repo\\src\\lib.rs")),
            (
                "alpha".to_string(),
                Some("C:\\repo\\src\\lib.rs".to_string())
            )
        );
    }
}
