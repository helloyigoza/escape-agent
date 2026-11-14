//! `ripwire --mcp` symbol-edit transport, receipt parsing and diff metadata.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::SymbolOp;
use super::file::{FileSnapshot, format_with_bom_eol, read_snapshot};
use super::resolve::{resolve_unique_candidate_abs, split_symbol_and_file};

use crate::code_map::{
    McpCallError, call_mcp_tool_with_scip, configured_ripwire_timeout, find_ripwire_binary,
    parse_mcp_json_text,
};
use crate::util;
use crate::{ToolContext, ToolError, ToolResult, diff};

fn build_preview_symbol_arg(symbol: &str, path_arg: Option<&str>) -> String {
    if symbol.starts_with('@') {
        return symbol.to_string();
    }
    if let Some(p) = path_arg
        && !symbol.contains(':')
    {
        return format!("{p}:{symbol}");
    }
    symbol.to_string()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn try_ripwire_symbol_edit(
    ctx: &ToolContext,
    root: &Path,
    scope_path: &Path,
    op: SymbolOp,
    symbol: &str,
    path_arg: Option<&str>,
    body_text: Option<&str>,
    post_check: bool,
) -> Result<Option<ToolResult>, ToolError> {
    let Some(binary) = find_ripwire_binary() else {
        return Ok(None);
    };

    let cancel = ctx.cancel.clone().unwrap_or_default();
    let timeout = configured_ripwire_timeout();

    if op == SymbolOp::Preview {
        let sym_arg = build_preview_symbol_arg(symbol, path_arg);
        let mut args = Map::new();
        args.insert("path".to_string(), json!(root));
        args.insert("symbol".to_string(), json!(sym_arg));
        if let Some(body) = body_text {
            args.insert("new_body".to_string(), json!(body));
        }
        return match call_mcp_tool_with_scip(
            &binary,
            root,
            None,
            &cancel,
            timeout,
            op.mcp_tool(),
            &Value::Object(args),
        )
        .await
        {
            Ok(out) if !out.text.trim().is_empty() => Ok(Some(
                ToolResult::text(out.text.trim().to_string()).with_metadata(json!({
                    "operation": "preview",
                    "engine": "ripwire",
                    "transport": "mcp",
                    "symbol": symbol,
                })),
            )),
            Ok(_) => Ok(None),
            Err(err) => handle_mcp_err("preview", err),
        };
    }

    // Best-effort pre-edit snapshot + intra-process lock if we can identify the file first.
    let candidate = resolve_unique_candidate_abs(ctx, root, scope_path, symbol, path_arg);
    let _lock = match candidate.as_ref() {
        Some(p) if p.is_file() => Some(util::lock_file(p).await),
        _ => None,
    };
    let before_snap = candidate
        .as_ref()
        .filter(|p| p.is_file())
        .and_then(|p| read_snapshot(&ctx.directory, p).ok());

    let (mcp_sym, mcp_file) = split_symbol_and_file(symbol, path_arg);
    let mut args = Map::new();
    args.insert("path".to_string(), json!(root));
    args.insert("symbol".to_string(), json!(mcp_sym));
    if let Some(f) = mcp_file {
        args.insert("file".to_string(), json!(f));
    }
    let body = body_text.unwrap_or_default();
    match op {
        SymbolOp::Replace => {
            args.insert("new_body".to_string(), json!(body));
        }
        SymbolOp::InsertBefore | SymbolOp::InsertAfter => {
            args.insert("text".to_string(), json!(body));
        }
        SymbolOp::Preview => {}
    }
    // IMPORTANT: ripwire 0.6.8 rejects `"post_check": "true"`; omit for default true,
    // pass `"false"` only when explicitly skipping checks.
    if !post_check {
        args.insert("post_check".to_string(), json!("false"));
    }

    match call_mcp_tool_with_scip(
        &binary,
        root,
        None,
        &cancel,
        timeout,
        op.mcp_tool(),
        &Value::Object(args),
    )
    .await
    {
        Ok(out) => {
            parse_ripwire_edit_receipt(ctx, root, op, symbol, &out.text, before_snap.as_ref())
        }
        Err(err) => handle_mcp_err(op.as_str(), err),
    }
}

fn handle_mcp_err(op: &str, err: McpCallError) -> Result<Option<ToolResult>, ToolError> {
    match err {
        McpCallError::Cancelled => Err(ToolError::new("Tool execution cancelled")),
        McpCallError::Timeout { timeout } => Err(ToolError::new(format!(
            "symbol_edit ({op}) timed out after {} ms",
            timeout.as_millis()
        ))),
        McpCallError::CliOnly { .. } | McpCallError::Io { .. } => Ok(None),
        McpCallError::Tool { message, .. } => {
            let low = message.to_ascii_lowercase();
            if low.contains("ambiguous") || low.contains("multiple") {
                return Err(ToolError::new(message));
            }
            // Allow native fallback when ripwire doesn't support this language/form.
            Ok(None)
        }
    }
}

fn parse_ripwire_edit_receipt(
    ctx: &ToolContext,
    root: &Path,
    op: SymbolOp,
    symbol: &str,
    raw: &str,
    before_snap: Option<&FileSnapshot>,
) -> Result<Option<ToolResult>, ToolError> {
    let Some(val) = parse_mcp_json_text(raw) else {
        return Ok(None);
    };
    if let Some(err_msg) = val.get("error").and_then(Value::as_str) {
        let low = err_msg.to_ascii_lowercase();
        if low.contains("ambiguous") || low.contains("multiple") || low.contains("not found") {
            return Err(ToolError::new(format!("symbol_edit: {err_msg}")));
        }
        return Ok(None);
    }
    if val.get("applied").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }

    let file_rel = val.get("file").and_then(Value::as_str).unwrap_or("");
    let (display_path, files_meta) = collect_ripwire_edit_diff(ctx, root, file_rel, before_snap);

    let resolved_sym = val.get("symbol").and_then(Value::as_str).unwrap_or(symbol);
    let mut summary = format!(
        "Applied `{}` on symbol `{resolved_sym}` in {display_path}.",
        op.as_str()
    );
    if let Some(lines) = val.get("lines").and_then(Value::as_str) {
        let _ = write!(summary, " (lines {lines})");
    }

    let contract_warnings = append_edit_check_warnings(&val, &mut summary);
    let tests_to_run: Vec<String> = val
        .get("tests_to_run")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if !tests_to_run.is_empty() {
        let _ = writeln!(summary, "\ntests_to_run: {}", tests_to_run.join(", "));
    }

    let mut metadata = json!({
        "operation": op.as_str(),
        "engine": "ripwire",
        "transport": "mcp",
        "symbol": resolved_sym,
        "path": display_path,
        "files": files_meta,
    });
    if !contract_warnings.is_empty() {
        metadata["contract_warnings"] = json!(contract_warnings);
    }
    if !tests_to_run.is_empty() {
        metadata["tests_to_run"] = json!(tests_to_run);
    }
    Ok(Some(ToolResult::text(summary).with_metadata(metadata)))
}

fn collect_ripwire_edit_diff(
    ctx: &ToolContext,
    root: &Path,
    file_rel: &str,
    before_snap: Option<&FileSnapshot>,
) -> (String, Vec<Value>) {
    let actual_abs = if file_rel.is_empty() {
        before_snap.map(|s| s.abs_path.clone())
    } else {
        let joined = root.join(file_rel);
        if joined.exists() {
            Some(joined)
        } else {
            let dir_joined = ctx.directory.join(file_rel);
            dir_joined.exists().then_some(dir_joined)
        }
    };

    let mut files_meta = Vec::new();
    let mut display_path = file_rel.to_string();
    if let Some(abs) = actual_abs.as_ref() {
        display_path = util::relative_display(&ctx.directory, abs);
        if let Ok(after_bytes) = fs::read(abs) {
            let (after_bom, after_dec) = util::decode_bytes(&after_bytes);
            let mut after_lf = after_dec.replace("\r\n", "\n");
            if let Some(before) = before_snap
                && canonical_eq(&before.abs_path, abs)
            {
                if before.crlf || before.bom != after_bom {
                    let restored = format_with_bom_eol(&after_lf, before.bom, before.crlf);
                    if util::atomic_write(abs, restored.as_bytes()).is_ok() {
                        after_lf = restored.trim_start_matches(util::BOM).replace("\r\n", "\n");
                    }
                }
                files_meta.push(diff::file_diff(
                    &before.display_rel,
                    &before.clean_lf,
                    &after_lf,
                    "updated",
                ));
            }
        }
    }
    (display_path, files_meta)
}

fn append_edit_check_warnings(val: &Value, summary: &mut String) -> Vec<String> {
    let mut contract_warnings = Vec::new();
    let Some(check) = val.get("edit_check") else {
        return contract_warnings;
    };
    let status = check.get("status").and_then(Value::as_str).unwrap_or("ok");
    if status != "ok" && status != "clean" {
        let msg = format!("edit_check status: {status}");
        contract_warnings.push(msg.clone());
        let _ = write!(summary, "\n{msg}");
    }
    if let Some(incompat) = check.get("incompatible").and_then(Value::as_array)
        && !incompat.is_empty()
    {
        let _ = writeln!(summary, "\nincompatible callers ({}):", incompat.len());
        for item in incompat.iter().take(6) {
            let text = item
                .as_str()
                .map_or_else(|| item.to_string(), ToString::to_string);
            contract_warnings.push(text.clone());
            let _ = writeln!(summary, "  - {text}");
        }
    }
    contract_warnings
}

fn canonical_eq(a: &Path, b: &Path) -> bool {
    let ca = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}
