//! `batch` operation plus the shared parsing/formatting helpers it uses
//! (batch items, handles, connect endpoints, ripwire JSON/XML projections).

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};
use tokio::process::Command;

use super::super::CodeMapTool;
use super::super::files::{read_bounded_file, relative_to};
use super::super::native_index::{
    native_affected, native_communities, native_connect, native_expand, native_impact, native_map,
    native_pack_task, native_symbol, native_trace, tree_root_for,
};
use super::super::parsers::parse_symbols;
use super::super::ripwire::{compact_ripwire_cli_output, strip_comments_and_blank_runs};
use super::mcp::{McpAttempt, build_mcp_result, matches_scope, run_mcp};
use crate::{ToolContext, ToolError, ToolResult};

impl CodeMapTool {
    pub(in crate::code_map) async fn execute_batch(
        ctx: &ToolContext,
        target_path: &Path,
        queries: &[String],
        max_tokens: usize,
        limit: usize,
        compress: bool,
    ) -> Result<ToolResult, ToolError> {
        if queries.is_empty() || queries.len() > 16 {
            return Err(ToolError::new(
                "`queries` must contain between 1 and 16 entries for operation 'batch'",
            ));
        }

        let parsed: Vec<BatchItem> = queries
            .iter()
            .map(|raw| parse_batch_item(raw))
            .collect::<Result<Vec<_>, _>>()?;

        let root = tree_root_for(ctx, target_path);
        if parsed.iter().all(BatchItem::supports_mcp_batch) {
            let mcp_queries: Vec<Value> =
                parsed.iter().map(|item| item.to_mcp_json(limit)).collect();
            if let Some(McpAttempt::Ok(out)) = run_mcp(
                ctx,
                &root,
                "batch",
                "batch",
                &json!({ "path": root, "queries": mcp_queries }),
            )
            .await?
                && !out.text.trim().is_empty()
            {
                let compacted = compact_ripwire_cli_output(&out.text);
                return Ok(build_mcp_result(
                    "batch",
                    compacted,
                    &out.stderr,
                    &json!({ "count": queries.len() }),
                ));
            }
        }

        // Native (or mixed) batched fallback: evaluate each sub-query locally.
        let mut combined = String::from("<batch>\n");
        let per_budget = (max_tokens / queries.len().max(1)).max(250);
        for (idx, item) in parsed.iter().enumerate() {
            let section = match item.verb.as_str() {
                "find_symbol" | "symbol" => {
                    native_symbol(ctx, target_path, &item.arg, limit).map(|(s, _)| s)
                }
                "impact" => native_impact(ctx, target_path, &item.arg, limit).map(|(s, _)| s),
                "uses" | "references" => {
                    Self::execute_references_native(ctx, target_path, &item.arg, limit)
                        .map(|r| r.output.unwrap_or_default())
                }
                "for" | "map" => native_map(ctx, target_path, &item.arg, None, per_budget, limit)
                    .map(|(s, _, _)| s),
                "explore" | "pack_task" => {
                    native_pack_task(ctx, target_path, &item.arg, per_budget, limit).map(|(s, _)| s)
                }
                "expand" | "fetch_body" => {
                    native_expand(ctx, target_path, &item.arg, None, None, compress).map(|(s, _)| s)
                }
                "connect" | "path_between" => {
                    let (a, b) = parse_connect_endpoints(&item.arg).ok_or_else(|| {
                        ToolError::new(format!("Invalid connect pair in batch: '{}'", item.arg))
                    })?;
                    native_connect(ctx, target_path, &a, &b).map(|(s, _)| s)
                }
                "affected" => {
                    let csv = if item.arg.is_empty() {
                        detect_git_changed_csv(&root).await
                    } else {
                        item.arg.clone()
                    };
                    native_affected(ctx, target_path, &csv, limit).map(|(s, _)| s)
                }
                "trace" | "from_trace" => {
                    native_trace(ctx, target_path, &item.arg, per_budget).map(|(s, _)| s)
                }
                "communities" => {
                    let filter = (!item.arg.is_empty()).then_some(item.arg.as_str());
                    native_communities(ctx, target_path, filter, limit).map(|(s, _)| s)
                }
                "grep" => Self::execute_references_native(ctx, target_path, &item.arg, limit)
                    .map(|r| r.output.unwrap_or_default()),
                other => Err(ToolError::new(format!("Unsupported batch verb '{other}'"))),
            }?;
            let _ = writeln!(
                combined,
                "<q i=\"{idx}\" verb=\"{}\">\n{}\n</q>",
                item.verb, section
            );
        }
        combined.push_str("</batch>");

        Ok(ToolResult::text(combined).with_metadata(json!({
            "operation": "batch",
            "engine": "native",
            "count": queries.len(),
        })))
    }
}

pub(super) struct BatchItem {
    pub(super) verb: String,
    pub(super) arg: String,
}

impl BatchItem {
    pub(super) fn supports_mcp_batch(&self) -> bool {
        matches!(
            self.verb.as_str(),
            "find_symbol"
                | "symbol"
                | "uses"
                | "references"
                | "impact"
                | "grep"
                | "for"
                | "map"
                | "explore"
                | "pack_task"
                | "connect"
                | "path_between"
                | "fetch_body"
                | "expand"
                | "from_trace"
                | "trace"
                | "affected"
        )
    }

    fn to_mcp_json(&self, limit: usize) -> Value {
        match self.verb.as_str() {
            "find_symbol" | "symbol" => {
                json!({ "verb": "find_symbol", "symbol": self.arg, "limit": limit })
            }
            "uses" | "references" => {
                json!({ "verb": "uses", "symbol": self.arg, "limit": limit })
            }
            "impact" => json!({ "verb": "impact", "symbol": self.arg, "limit": limit }),
            "grep" => json!({ "verb": "grep", "pattern": self.arg, "limit": limit, "in": "any" }),
            "for" | "map" => json!({ "verb": "for", "task": self.arg, "limit": limit }),
            "explore" | "pack_task" => {
                json!({ "verb": "explore", "task": self.arg, "limit": limit })
            }
            "fetch_body" | "expand" => {
                let handle =
                    resolve_direct_handle(&self.arg, None).unwrap_or_else(|| self.arg.clone());
                json!({ "verb": "fetch_body", "handle": handle })
            }
            "from_trace" | "trace" => json!({ "verb": "from_trace", "trace": self.arg }),
            "affected" => json!({ "verb": "affected", "file": self.arg }),
            "connect" | "path_between" => {
                let (from, to) = parse_connect_endpoints(&self.arg)
                    .unwrap_or_else(|| (self.arg.clone(), String::new()));
                json!({ "verb": "path_between", "from": from, "to": to })
            }
            _ => json!({ "verb": self.verb, "symbol": self.arg }),
        }
    }
}

pub(super) fn parse_batch_item(raw: &str) -> Result<BatchItem, ToolError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ToolError::new("Empty sub-query in `batch` `queries`"));
    }
    if let Some((verb, arg)) = trimmed.split_once(':') {
        return Ok(BatchItem {
            verb: verb.trim().to_lowercase(),
            arg: arg.trim().to_string(),
        });
    }
    Ok(BatchItem {
        verb: trimmed.to_lowercase(),
        arg: String::new(),
    })
}

pub(super) fn parse_connect_endpoints(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    for sep in ["->", "..", ","] {
        if let Some((a, b)) = trimmed.split_once(sep) {
            let left = a.trim().to_string();
            let right = b.trim().to_string();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, right));
            }
        }
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() == 2 {
        return Some((parts[0].to_string(), parts[1].to_string()));
    }
    None
}

pub(super) fn resolve_direct_handle(query: &str, rel_scope: Option<&str>) -> Option<String> {
    let q = query.trim();
    if q.starts_with("sym#") || q.starts_with('@') {
        return Some(q.to_string());
    }
    if let Some((file_part, rest)) = q.rsplit_once(':')
        && rest.trim().parse::<usize>().is_ok()
    {
        let file = match rel_scope {
            Some(prefix) if !file_part.contains('/') && !prefix.contains('.') => {
                format!("{prefix}/{}", file_part.trim())
            }
            _ => file_part.trim().to_string(),
        };
        return Some(format!("@{file}:{}", rest.trim()));
    }
    None
}

pub(super) fn extract_handle_from_find_symbol(
    json_text: &str,
    rel_scope: Option<&str>,
) -> Option<String> {
    let val: Value = serde_json::from_str(json_text).ok()?;
    let sym = val.get("symbol")?;
    let file = sym.get("file").and_then(Value::as_str)?;
    if !matches_scope(file, rel_scope) {
        return None;
    }
    sym.get("handle")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

pub(super) fn format_find_symbol_json(
    ctx: &ToolContext,
    root: &Path,
    rel_scope: Option<&str>,
    query: &str,
    json_text: &str,
) -> Option<String> {
    let val: Value = serde_json::from_str(json_text).ok()?;
    let sym = val.get("symbol")?;
    let rel_file = sym.get("file").and_then(Value::as_str)?;
    if !matches_scope(rel_file, rel_scope) {
        return None;
    }
    let name = sym.get("name").and_then(Value::as_str).unwrap_or(query);
    let raw_kind = sym.get("kind").and_then(Value::as_str).unwrap_or("symbol");
    let line = usize::try_from(sym.get("line").and_then(Value::as_u64).unwrap_or(1)).unwrap_or(1);
    let handle = sym.get("handle").and_then(Value::as_str).unwrap_or("");

    let abs_file = root.join(rel_file);
    let display_rel = relative_to(&ctx.directory, &abs_file);

    let (norm_kind, signature) = resolve_kind_and_signature(&abs_file, name, raw_kind, line);

    let mut out = format!("{display_rel}:{line} [{norm_kind}] {signature}");
    if !handle.is_empty() {
        let _ = write!(out, " | handle={handle}");
    }

    if let Some(callers) = val.get("calledBy").and_then(Value::as_array)
        && !callers.is_empty()
    {
        let names: Vec<&str> = callers
            .iter()
            .take(5)
            .filter_map(|c| c.get("name").and_then(Value::as_str))
            .collect();
        if !names.is_empty() {
            let _ = write!(out, " | callers:[{}]", names.join(","));
        }
    }
    if let Some(calls) = val.get("calls").and_then(Value::as_array)
        && !calls.is_empty()
    {
        let names: Vec<&str> = calls
            .iter()
            .take(5)
            .filter_map(|c| c.get("name").and_then(Value::as_str))
            .collect();
        if !names.is_empty() {
            let _ = write!(out, " | calls:[{}]", names.join(","));
        }
    }
    Some(out)
}

fn resolve_kind_and_signature(
    abs_file: &Path,
    name: &str,
    raw_kind: &str,
    line: usize,
) -> (String, String) {
    if let Ok(content) = read_bounded_file(abs_file) {
        let ext = abs_file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let parsed = parse_symbols(&content, ext);
        if let Some(sym) = parsed
            .iter()
            .find(|s| s.name == name && s.line.abs_diff(line) <= 2)
        {
            return (sym.kind.as_str().to_string(), sym.signature.clone());
        }
        if let Some(src_line) = content.lines().nth(line.saturating_sub(1)) {
            let normalized = match raw_kind {
                "iface" => "interface",
                "cls" if src_line.contains("struct ") => "struct",
                "cls" if src_line.contains("enum ") => "enum",
                "cls" if src_line.contains("trait ") => "trait",
                "cls" => "class",
                other => other,
            };
            return (normalized.to_string(), src_line.trim().to_string());
        }
    }
    let normalized = match raw_kind {
        "iface" => "interface",
        "cls" => "class",
        other => other,
    };
    (normalized.to_string(), name.to_string())
}

pub(super) fn format_fetch_body_json(
    ctx: &ToolContext,
    root: &Path,
    json_text: &str,
    compress: bool,
) -> Option<(String, String)> {
    let val: Value = serde_json::from_str(json_text).ok()?;
    let rel_file = val.get("file").and_then(Value::as_str)?;
    let name = val.get("name").and_then(Value::as_str).unwrap_or("symbol");
    let kind = val.get("kind").and_then(Value::as_str).unwrap_or("symbol");
    let def_line = val.get("line").and_then(Value::as_u64).unwrap_or(1);
    let start_off = val.get("start_line").and_then(Value::as_u64).unwrap_or(1);
    let end_off = val
        .get("end_line")
        .and_then(Value::as_u64)
        .unwrap_or(start_off);
    let raw_body = val.get("body").and_then(Value::as_str)?;

    let body = if compress {
        strip_comments_and_blank_runs(raw_body)
    } else {
        raw_body.to_string()
    };

    let abs_file = root.join(rel_file);
    let display_rel = relative_to(&ctx.directory, &abs_file);
    let abs_start = def_line.saturating_add(start_off.saturating_sub(1));
    let abs_end = def_line.saturating_add(end_off.saturating_sub(1));
    let ext = abs_file.extension().and_then(|e| e.to_str()).unwrap_or("");

    let text = format!(
        "Symbol: {name} [{kind}] ({display_rel}:{abs_start}..{abs_end})\n```{ext}\n{body}\n```"
    );
    Some((text, display_rel))
}

pub(super) fn format_uses_xml(
    ctx: &ToolContext,
    root: &Path,
    rel_scope: Option<&str>,
    query: &str,
    xml_text: &str,
) -> Option<(String, usize)> {
    let mut rows = Vec::new();
    let mut rest = xml_text;
    while let Some(pos) = rest.find("<u ") {
        rest = &rest[pos..];
        let Some(end) = rest.find("/>") else {
            break;
        };
        let tag = &rest[..end + 2];
        rest = &rest[end + 2..];

        if let Some(p_val) = extract_xml_attr(tag, "p") {
            let file_only = p_val.rsplit_once(':').map_or(p_val, |(f, _)| f);
            if !matches_scope(file_only, rel_scope) {
                continue;
            }
            let role = extract_xml_attr(tag, "role").unwrap_or("use");
            let in_id = extract_xml_attr(tag, "in_id").unwrap_or("");
            let line_part = p_val.rsplit_once(':').map_or("", |(_, l)| l);
            let display_rel = relative_to(&ctx.directory, &root.join(file_only));
            rows.push(format!(
                "  {display_rel}:{line_part:<4} [{role}] in {in_id}"
            ));
        }
    }

    if rows.is_empty() {
        return None;
    }
    let total = rows.len();
    let out = format!(
        "References to \"{query}\" ({total} hits):\n{}",
        rows.join("\n")
    );
    Some((out, total))
}

pub(super) fn format_grep_json(
    ctx: &ToolContext,
    root: &Path,
    rel_scope: Option<&str>,
    query: &str,
    json_text: &str,
    limit: usize,
) -> Option<(String, usize)> {
    let val: Value = serde_json::from_str(json_text).ok()?;
    let hits = val.get("hits").and_then(Value::as_array)?;
    let mut rows = Vec::new();
    for hit in hits {
        let Some(file) = hit.get("file").and_then(Value::as_str) else {
            continue;
        };
        if !matches_scope(file, rel_scope) {
            continue;
        }
        let line = hit.get("line").and_then(Value::as_u64).unwrap_or(1);
        let in_sym = hit.get("in").and_then(Value::as_str).unwrap_or("");
        let text = hit.get("text").and_then(Value::as_str).unwrap_or("").trim();
        let display_rel = relative_to(&ctx.directory, &root.join(file));
        if in_sym.is_empty() {
            rows.push(format!("  {display_rel}:{line:<4} {text}"));
        } else {
            rows.push(format!("  {display_rel}:{line:<4} [{in_sym}] {text}"));
        }
        if rows.len() >= limit {
            break;
        }
    }
    if rows.is_empty() {
        return None;
    }
    let total = rows.len();
    let out = format!(
        "References to \"{query}\" ({total} hits):\n{}",
        rows.join("\n")
    );
    Some((out, total))
}

pub(in crate::code_map) fn extract_xml_attr<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(&tag[start..start + end])
}

pub(super) fn truncate_budget(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars.max(200);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(nl) = text[..end].rfind('\n')
        && nl > max_chars / 2
    {
        end = nl;
    }
    format!("{}\n<!-- truncated to {max_chars} chars -->", &text[..end])
}

pub(super) async fn detect_git_changed_csv(root: &Path) -> String {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain"])
        .output()
        .await
    else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let paths: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.get(3..))
        .map(|p| {
            let trimmed = p.trim();
            trimmed
                .split_once(" -> ")
                .map_or(trimmed, |(_, new_p)| new_p.trim())
                .trim_matches('"')
                .to_string()
        })
        .filter(|p| !p.is_empty())
        .collect();
    paths.join(",")
}
