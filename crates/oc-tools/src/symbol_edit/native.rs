//! Pure-Rust AST-span `symbol_edit` engine (splice, preview, atomic write).

use std::fmt::Write as _;
use std::path::Path;

use serde_json::json;

use super::SymbolOp;
use super::file::{format_with_bom_eol, read_snapshot};
use super::resolve::{pick_single_location, resolve_native_target};

use crate::code_map::relative_to;
use crate::util;
use crate::{ToolContext, ToolError, ToolResult, diff};

pub(super) async fn execute_native_symbol_edit(
    ctx: &ToolContext,
    root: &Path,
    scope_path: &Path,
    op: SymbolOp,
    symbol: &str,
    path_arg: Option<&str>,
    body_text: Option<&str>,
) -> Result<ToolResult, ToolError> {
    let locs = resolve_native_target(ctx, root, scope_path, symbol, path_arg)?;
    let target = pick_single_location(ctx, &locs, symbol)?;

    if op == SymbolOp::Preview {
        let snap = read_snapshot(&ctx.directory, &target.abs_path)?;
        let span_lines = extract_line_range(&snap.clean_lf, target.start_line, target.end_line);
        let mut out = format!(
            "Preview for `{}` at {}:{}-{} [{}]:\nCurrent signature: {}\nSpan bytes/lines:\n{}",
            target.name,
            snap.display_rel,
            target.start_line,
            target.end_line,
            target.kind.as_str(),
            target.signature,
            span_lines,
        );
        if let Some(candidate) = body_text {
            let _ = write!(out, "\nProposed replacement:\n{}", candidate.trim_end());
        }
        return Ok(ToolResult::text(out).with_metadata(json!({
            "operation": "preview",
            "engine": "native",
            "symbol": target.name,
            "path": snap.display_rel,
            "start_line": target.start_line,
            "end_line": target.end_line,
        })));
    }

    let _guard = util::lock_file(&target.abs_path).await;
    let snap = read_snapshot(&ctx.directory, &target.abs_path)?;
    let content_lf = body_text.unwrap_or_default().replace("\r\n", "\n");
    let after_lf = splice_symbol_span(
        &snap.clean_lf,
        target.start_line,
        target.end_line,
        op,
        &content_lf,
    )?;

    if after_lf == snap.clean_lf {
        return Ok(ToolResult::text(format!(
            "No changes required for `{}` in {}.",
            target.name, snap.display_rel
        ))
        .with_metadata(json!({
            "operation": op.as_str(),
            "engine": "native",
            "symbol": target.name,
            "path": snap.display_rel,
            "files": [],
        })));
    }

    let encoded = format_with_bom_eol(&after_lf, snap.bom, snap.crlf);
    util::atomic_write(&target.abs_path, encoded.as_bytes())
        .map_err(|e| ToolError::new(format!("Failed to write {}: {e}", snap.display_rel)))?;

    let diff_entry = diff::file_diff(&snap.display_rel, &snap.clean_lf, &after_lf, "updated");
    let display_rel = relative_to(root, &target.abs_path);
    let summary = format!(
        "Applied `{}` on `{}` in {} (lines {}..={}).",
        op.as_str(),
        target.name,
        snap.display_rel,
        target.start_line,
        target.end_line
    );

    Ok(ToolResult::text(summary).with_metadata(json!({
        "operation": op.as_str(),
        "engine": "native",
        "symbol": target.name,
        "path": snap.display_rel,
        "repo_rel": display_rel,
        "files": [diff_entry],
    })))
}

fn splice_symbol_span(
    source_lf: &str,
    start_line: usize,
    end_line: usize,
    op: SymbolOp,
    content_lf: &str,
) -> Result<String, ToolError> {
    let lines: Vec<&str> = source_lf.split_inclusive('\n').collect();
    if start_line == 0 || start_line > lines.len() || end_line < start_line {
        return Err(ToolError::new(format!(
            "Invalid symbol line span {start_line}..={end_line} for file with {} lines",
            lines.len()
        )));
    }
    let capped_end = end_line.min(lines.len());
    let start_idx = start_line - 1;
    let end_idx_excl = capped_end;

    let prefix_bytes: usize = lines[..start_idx].iter().map(|l| l.len()).sum();
    let span_end_bytes: usize = lines[..end_idx_excl].iter().map(|l| l.len()).sum();

    let span_ended_with_nl = source_lf[..span_end_bytes].ends_with('\n');
    let mut normalized = content_lf.to_string();
    if (op == SymbolOp::InsertBefore || span_ended_with_nl) && !normalized.ends_with('\n') {
        normalized.push('\n');
    }

    let mut out = String::with_capacity(source_lf.len() + normalized.len() + 2);
    match op {
        SymbolOp::Replace => {
            out.push_str(&source_lf[..prefix_bytes]);
            out.push_str(&normalized);
            out.push_str(&source_lf[span_end_bytes..]);
        }
        SymbolOp::InsertBefore => {
            out.push_str(&source_lf[..prefix_bytes]);
            out.push_str(&normalized);
            out.push_str(&source_lf[prefix_bytes..]);
        }
        SymbolOp::InsertAfter => {
            out.push_str(&source_lf[..span_end_bytes]);
            if !source_lf[..span_end_bytes].ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&normalized);
            out.push_str(&source_lf[span_end_bytes..]);
        }
        SymbolOp::Preview => return Ok(source_lf.to_string()),
    }
    Ok(out)
}

fn extract_line_range(source_lf: &str, start_line: usize, end_line: usize) -> String {
    source_lf
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let ln = idx + 1;
            (ln >= start_line && ln <= end_line).then(|| format!("L{ln}: {line}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
