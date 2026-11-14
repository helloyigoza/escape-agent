//! Deep-analysis `code_map` operations: `references`, `lego`, `slice`,
//! `cochange`, `doc_drift`, `flags` and `quality` (CLI + MCP variants).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::CodeMapTool;
use super::super::files::relative_to;
use super::super::native_index::{
    native_cochange, native_doc_drift, native_flags, native_lego, native_quality, native_slice,
    relative_scope, tree_root_for,
};
use super::super::precision::{PrecisionMode, scip_for_mode};
use super::super::ripwire::{
    RipwireFailure, compact_ripwire_cli_output, find_ripwire_binary, run_ripwire_args,
    trimmed_stderr,
};
use super::exec_batch::{format_grep_json, format_uses_xml, truncate_budget};
use super::mcp::{
    McpAttempt, add_fallback_metadata, finalize_precision, mcp_metadata, record_scip_meta, run_mcp,
    run_mcp_mode, timeout_error,
};
use crate::{ToolContext, ToolError, ToolResult};

impl CodeMapTool {
    pub(in crate::code_map) async fn execute_references(
        ctx: &ToolContext,
        target_path: &Path,
        query: &str,
        limit: usize,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let rel_scope = relative_scope(&root, target_path);

        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "references",
            "uses",
            &json!({ "path": root, "symbol": query, "limit": limit }),
        )
        .await?;
        if let Some(McpAttempt::Ok(uses_out)) = attempt {
            if let Some((formatted, matches)) =
                format_uses_xml(ctx, &root, rel_scope.as_deref(), query, &uses_out.text)
                && matches > 0
            {
                let metadata = mcp_metadata(
                    "references",
                    &uses_out.stderr,
                    &json!({ "query": query, "matches": matches }),
                );
                return Ok(finalize_precision(
                    ctx,
                    target_path,
                    "references",
                    query,
                    precision,
                    used_scip,
                    formatted,
                    metadata,
                )
                .await);
            }

            // Automatically fall back to trigram literal grep for string literals / non-call uses.
            if let Some(McpAttempt::Ok(grep_out)) = run_mcp(
                ctx,
                &root,
                "references",
                "grep",
                &json!({ "path": root, "pattern": query, "limit": limit, "in": "any" }),
            )
            .await?
                && let Some((formatted, matches)) = format_grep_json(
                    ctx,
                    &root,
                    rel_scope.as_deref(),
                    query,
                    &grep_out.text,
                    limit,
                )
                && matches > 0
            {
                let metadata = mcp_metadata(
                    "references",
                    &grep_out.stderr,
                    &json!({ "query": query, "matches": matches }),
                );
                return Ok(finalize_precision(
                    ctx,
                    target_path,
                    "references",
                    query,
                    precision,
                    used_scip,
                    formatted,
                    metadata,
                )
                .await);
            }
        }

        let res = Self::execute_references_native(ctx, target_path, query, limit)?;
        Ok(finalize_precision(
            ctx,
            target_path,
            "references",
            query,
            precision,
            false,
            res.output.unwrap_or_default(),
            res.metadata
                .unwrap_or_else(|| json!({ "operation": "references" })),
        )
        .await)
    }

    pub(in crate::code_map) async fn execute_lego(
        ctx: &ToolContext,
        target_path: &Path,
        query: &str,
        limit: usize,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "lego",
            "lego",
            &json!({ "path": root, "type": query, "limit": limit }),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && !out.text.trim().is_empty()
        {
            let metadata = mcp_metadata("lego", &out.stderr, &json!({ "query": query }));
            return Ok(finalize_precision(
                ctx,
                target_path,
                "lego",
                query,
                precision,
                used_scip,
                compact_ripwire_cli_output(&out.text),
                metadata,
            )
            .await);
        }

        let (text, implementors) = native_lego(ctx, target_path, query, limit)?;
        let metadata = json!({
            "operation": "lego",
            "engine": "native",
            "query": query,
            "implementors": implementors,
        });
        Ok(finalize_precision(
            ctx,
            target_path,
            "lego",
            query,
            precision,
            false,
            text,
            metadata,
        )
        .await)
    }

    pub(in crate::code_map) async fn execute_slice(
        ctx: &ToolContext,
        target_path: &Path,
        query: &str,
        var: Option<&str>,
        flow: Option<&str>,
        depth: Option<usize>,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let mut args = Map::new();
        args.insert("path".to_string(), json!(root));
        args.insert("symbol".to_string(), json!(query));
        if let Some(v) = var.map(str::trim).filter(|s| !s.is_empty()) {
            args.insert("var".to_string(), json!(v));
        }
        if let Some(f) = flow.map(str::trim).filter(|s| !s.is_empty()) {
            args.insert("flow".to_string(), json!(f));
        }
        if let Some(d) = depth {
            args.insert("depth".to_string(), json!(d.clamp(1, 32)));
        }

        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "slice",
            "slice",
            &Value::Object(args),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && !out.text.trim().is_empty()
        {
            let mut extra = json!({ "query": query });
            if let Some(v) = var {
                extra["var"] = json!(v);
            }
            if let Some(f) = flow {
                extra["flow"] = json!(f);
            }
            let metadata = mcp_metadata("slice", &out.stderr, &extra);
            return Ok(finalize_precision(
                ctx,
                target_path,
                "slice",
                query,
                precision,
                used_scip,
                compact_ripwire_cli_output(&out.text),
                metadata,
            )
            .await);
        }

        let (text, hits) = native_slice(ctx, target_path, query, var, flow, depth.unwrap_or(8))?;
        let mut metadata = json!({
            "operation": "slice",
            "engine": "native",
            "query": query,
            "hits": hits,
        });
        if let Some(v) = var {
            metadata["var"] = json!(v);
        }
        if let Some(f) = flow {
            metadata["flow"] = json!(f);
        }
        Ok(finalize_precision(
            ctx,
            target_path,
            "slice",
            query,
            precision,
            false,
            text,
            metadata,
        )
        .await)
    }

    pub(in crate::code_map) async fn execute_cochange(
        ctx: &ToolContext,
        target_path: &Path,
        query: Option<&str>,
        limit: usize,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let q_file = query.map(str::trim).filter(|s| !s.is_empty());
        let (mcp_file, native_target): (String, PathBuf) = if let Some(q) = q_file {
            let candidate = ctx.directory.join(q);
            let resolved = if candidate.exists() {
                candidate
            } else {
                root.join(q)
            };
            (q.to_string(), resolved)
        } else if target_path.is_file() {
            (relative_to(&root, target_path), target_path.to_path_buf())
        } else {
            return Err(ToolError::new(
                "Operation 'cochange' requires a target file via `path` or `query`.",
            ));
        };

        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "cochange",
            "cochange",
            &json!({ "path": root, "file": mcp_file, "limit": limit }),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && !out.text.trim().is_empty()
        {
            let mut metadata = mcp_metadata("cochange", &out.stderr, &json!({ "file": mcp_file }));
            record_scip_meta(&mut metadata, precision, used_scip);
            return Ok(
                ToolResult::text(compact_ripwire_cli_output(&out.text)).with_metadata(metadata)
            );
        }

        let (text, partners) = native_cochange(ctx, &native_target, limit);
        let mut metadata = json!({
            "operation": "cochange",
            "engine": "native",
            "file": mcp_file,
            "partners": partners,
        });
        record_scip_meta(&mut metadata, precision, false);
        Ok(ToolResult::text(text).with_metadata(metadata))
    }

    pub(in crate::code_map) async fn execute_doc_drift(
        ctx: &ToolContext,
        target_path: &Path,
        query: Option<&str>,
        limit: usize,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let filter = query.map(str::trim).filter(|s| !s.is_empty());
        let mut args = Map::new();
        args.insert("path".to_string(), json!(root));
        args.insert("limit".to_string(), json!(limit));
        if let Some(k) = filter {
            args.insert("kind".to_string(), json!(k));
        }

        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "doc_drift",
            "doc_drift",
            &Value::Object(args),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && !out.text.trim().is_empty()
        {
            let mut metadata = mcp_metadata("doc_drift", &out.stderr, &json!({}));
            record_scip_meta(&mut metadata, precision, used_scip);
            return Ok(
                ToolResult::text(compact_ripwire_cli_output(&out.text)).with_metadata(metadata)
            );
        }

        let (text, findings) = native_doc_drift(ctx, target_path, filter, limit)?;
        let mut metadata = json!({
            "operation": "doc_drift",
            "engine": "native",
            "findings": findings,
        });
        record_scip_meta(&mut metadata, precision, false);
        Ok(ToolResult::text(text).with_metadata(metadata))
    }

    pub(in crate::code_map) async fn execute_flags(
        ctx: &ToolContext,
        target_path: &Path,
        query: Option<&str>,
        limit: usize,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let filter = query.map(str::trim).filter(|s| !s.is_empty());
        let mut args = Map::new();
        args.insert("path".to_string(), json!(root));
        args.insert("limit".to_string(), json!(limit));
        if let Some(k) = filter {
            args.insert("kind".to_string(), json!(k));
        }

        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "flags",
            "flags",
            &Value::Object(args),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && !out.text.trim().is_empty()
        {
            let mut metadata = mcp_metadata("flags", &out.stderr, &json!({}));
            record_scip_meta(&mut metadata, precision, used_scip);
            return Ok(
                ToolResult::text(compact_ripwire_cli_output(&out.text)).with_metadata(metadata)
            );
        }

        let (text, gates) = native_flags(ctx, target_path, filter, limit)?;
        let mut metadata = json!({
            "operation": "flags",
            "engine": "native",
            "gates": gates,
        });
        record_scip_meta(&mut metadata, precision, false);
        Ok(ToolResult::text(text).with_metadata(metadata))
    }

    pub(in crate::code_map) async fn execute_quality(
        ctx: &ToolContext,
        target_path: &Path,
        mode_query: Option<&str>,
        max_tokens: usize,
        limit: usize,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let mode = mode_query
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("delta");
        let flag = match mode {
            "delta" => None,
            "dead_code" => Some("--dead-code"),
            "clones" => Some("--clones"),
            "hotspots" => Some("--hotspots"),
            "deps" => Some("--deps"),
            "seams" => Some("--seams"),
            other => {
                return Err(ToolError::new(format!(
                    "Invalid quality lens '{other}'. Expected 'delta', 'dead_code', \
                     'clones', 'hotspots', 'deps', or 'seams'."
                )));
            }
        };

        let root = tree_root_for(ctx, target_path);
        let binary = find_ripwire_binary();
        let (hit, failure, stderr_note) = if let Some(cli_flag) = flag {
            try_quality_cli(
                ctx,
                binary.as_deref(),
                &root,
                cli_flag,
                mode,
                max_tokens,
                precision,
            )
            .await?
        } else {
            try_quality_delta_mcp(ctx, &root, mode, max_tokens, precision).await?
        };
        if let Some(res) = hit {
            return Ok(res);
        }

        let (mut out, findings) = native_quality(ctx, target_path, mode, limit)?;
        let mut metadata = json!({
            "operation": "quality",
            "mode": mode,
            "engine": "native",
            "findings": findings,
        });
        record_scip_meta(&mut metadata, precision, false);
        add_fallback_metadata(
            &mut out,
            &mut metadata,
            binary.is_none(),
            failure,
            stderr_note,
        );
        Ok(ToolResult::text(out).with_metadata(metadata))
    }
}

async fn try_quality_cli(
    ctx: &ToolContext,
    binary: Option<&Path>,
    root: &Path,
    cli_flag: &str,
    mode: &str,
    max_tokens: usize,
    precision: PrecisionMode,
) -> Result<(Option<ToolResult>, Option<String>, Option<String>), ToolError> {
    let Some(bin) = binary else {
        return Ok((None, None, None));
    };
    let mut args = vec![OsString::from(root.as_os_str()), OsString::from(cli_flag)];
    let scip = scip_for_mode(root, precision);
    if let Some(scip_path) = scip.as_ref() {
        args.push(OsString::from(format!(
            "--scip={}",
            scip_path.to_string_lossy()
        )));
    }
    let cancel = ctx.cancel.clone().unwrap_or_default();
    let cli_res = tokio::select! {
        () = cancel.cancelled() => return Err(ToolError::new("Tool execution cancelled")),
        res = run_ripwire_args(bin, &args) => res,
    };
    match cli_res {
        Ok(run) => {
            let stderr_note = trimmed_stderr(&run.stderr);
            let trimmed = run.stdout.trim();
            if !trimmed.is_empty() {
                let compacted = compact_ripwire_cli_output(trimmed);
                let budgeted = truncate_budget(&compacted, max_tokens.saturating_mul(4));
                let mut metadata = json!({
                    "operation": "quality",
                    "mode": mode,
                    "engine": "ripwire",
                    "transport": "cli",
                    "token_budget": max_tokens,
                });
                record_scip_meta(&mut metadata, precision, scip.is_some());
                if let Some(err) = stderr_note {
                    metadata["ripwire_stderr"] = json!(err);
                }
                return Ok((
                    Some(ToolResult::text(budgeted).with_metadata(metadata)),
                    None,
                    None,
                ));
            }
            Ok((
                None,
                Some(format!("ripwire {cli_flag} returned empty output")),
                stderr_note,
            ))
        }
        Err(RipwireFailure::Timeout { timeout }) => Err(timeout_error("quality", timeout)),
        Err(RipwireFailure::Spawn { message }) => Ok((None, Some(message), None)),
        Err(RipwireFailure::Exit { code, stderr }) => Ok((
            None,
            Some(format!(
                "ripwire exited with status {}",
                code.map_or_else(|| "signal".to_string(), |c| c.to_string())
            )),
            trimmed_stderr(&stderr),
        )),
    }
}

async fn try_quality_delta_mcp(
    ctx: &ToolContext,
    root: &Path,
    mode: &str,
    max_tokens: usize,
    precision: PrecisionMode,
) -> Result<(Option<ToolResult>, Option<String>, Option<String>), ToolError> {
    let (attempt, used_scip) = run_mcp_mode(
        ctx,
        root,
        precision,
        "quality",
        "quality_delta",
        &json!({ "path": root }),
    )
    .await?;
    match attempt {
        Some(McpAttempt::Ok(out)) if !out.text.trim().is_empty() => {
            let compacted = compact_ripwire_cli_output(&out.text);
            let budgeted = truncate_budget(&compacted, max_tokens.saturating_mul(4));
            let mut metadata = mcp_metadata("quality", &out.stderr, &json!({ "mode": mode }));
            record_scip_meta(&mut metadata, precision, used_scip);
            Ok((
                Some(ToolResult::text(budgeted).with_metadata(metadata)),
                None,
                None,
            ))
        }
        Some(McpAttempt::Ok(out)) => Ok((
            None,
            Some("ripwire quality_delta returned empty output".to_string()),
            trimmed_stderr(&out.stderr),
        )),
        Some(McpAttempt::Fallback { error, stderr }) => {
            Ok((None, Some(error), trimmed_stderr(&stderr)))
        }
        Some(McpAttempt::CliOnly { stderr }) => Ok((
            None,
            Some("ripwire binary does not support MCP quality_delta".to_string()),
            trimmed_stderr(&stderr),
        )),
        None => Ok((None, None, None)),
    }
}
