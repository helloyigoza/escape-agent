//! Query-shaped `code_map` operations: `pack_task`, `communities`, `symbol`,
//! `expand`, `impact`, `connect`, `affected` and `trace`.

use std::ffi::OsString;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::super::CodeMapTool;
use super::super::native_index::{
    native_affected, native_communities, native_connect, native_expand, native_impact,
    native_pack_task, native_symbol, native_trace, relative_scope, tree_root_for,
};
use super::super::precision::PrecisionMode;
use super::super::radar::partition_task;
use super::super::ripwire::{
    RipwireFailure, compact_ripwire_cli_output, find_ripwire_binary, run_ripwire_args,
    trimmed_stderr,
};
use super::exec_batch::{
    detect_git_changed_csv, extract_handle_from_find_symbol, format_fetch_body_json,
    format_find_symbol_json, parse_connect_endpoints, resolve_direct_handle, truncate_budget,
};
use super::mcp::{
    McpAttempt, add_fallback_metadata, build_mcp_result, finalize_precision, mcp_metadata,
    record_scip_meta, run_mcp, run_mcp_mode, timeout_error,
};
use crate::{ToolContext, ToolError, ToolResult};

impl CodeMapTool {
    pub(in crate::code_map) async fn execute_pack_task(
        ctx: &ToolContext,
        target_path: &Path,
        query: &str,
        max_tokens: usize,
        limit: usize,
        partitions: Option<usize>,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        if let Some(part_count) = partitions {
            let partitioned =
                partition_task(ctx, target_path, query, part_count, max_tokens).await?;
            let text = partitioned.format_for_tool();
            let mut metadata = json!({
                "operation": "pack_task",
                "engine": partitioned.engine,
                "query": query,
                "token_budget": max_tokens,
                "partitions": part_count,
            });
            if let (Some(dst), Some(src)) = (
                metadata.as_object_mut(),
                partitioned.metadata_summary().as_object(),
            ) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            return Ok(ToolResult::text(text).with_metadata(metadata));
        }

        let root = tree_root_for(ctx, target_path);
        let rel_scope = relative_scope(&root, target_path);
        let scoped_task = match &rel_scope {
            Some(rel) => format!("{query} (scope: {rel})"),
            None => query.to_string(),
        };

        let mut failure = None;
        let mut stderr = None;

        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "pack_task",
            "explore",
            &json!({
                "path": root,
                "task": scoped_task,
                "budget_tokens": max_tokens,
            }),
        )
        .await?;

        if let Some(attempt) = attempt {
            match attempt {
                McpAttempt::Ok(out) if !out.text.trim().is_empty() => {
                    let compacted = compact_ripwire_cli_output(&out.text);
                    let mut res = build_mcp_result(
                        "pack_task",
                        compacted,
                        &out.stderr,
                        &json!({ "query": query, "token_budget": max_tokens }),
                    );
                    if let Some(meta) = res.metadata.as_mut() {
                        record_scip_meta(meta, precision, used_scip);
                    }
                    return Ok(res);
                }
                McpAttempt::Ok(out) => {
                    stderr = trimmed_stderr(&out.stderr);
                    failure = Some("ripwire explore returned empty output".to_string());
                }
                McpAttempt::Fallback { error, stderr: err } => {
                    stderr = trimmed_stderr(&err);
                    failure = Some(error);
                }
                McpAttempt::CliOnly { stderr: err } => {
                    stderr = trimmed_stderr(&err);
                    failure = Some("ripwire binary does not support MCP explore".to_string());
                }
            }
        }

        let (mut out, sigs) = native_pack_task(ctx, target_path, query, max_tokens, limit)?;
        let mut metadata = json!({
            "operation": "pack_task",
            "engine": "native",
            "query": query,
            "token_budget": max_tokens,
            "symbols": sigs,
        });
        add_fallback_metadata(
            &mut out,
            &mut metadata,
            find_ripwire_binary().is_none(),
            failure,
            stderr,
        );
        Ok(ToolResult::text(out).with_metadata(metadata))
    }

    pub(in crate::code_map) async fn execute_communities(
        ctx: &ToolContext,
        target_path: &Path,
        query: Option<&str>,
        max_tokens: usize,
        limit: usize,
    ) -> Result<ToolResult, ToolError> {
        let binary = find_ripwire_binary();
        let root = tree_root_for(ctx, target_path);
        let mut failure = None;
        let mut stderr_note = None;

        if let Some(bin) = binary.as_ref() {
            let mut args = vec![
                OsString::from(root.as_os_str()),
                OsString::from("--communities"),
            ];
            if let Some(q) = query.map(str::trim).filter(|q| !q.is_empty())
                && q.chars().all(|c| c.is_ascii_digit())
            {
                args.pop();
                args.push(OsString::from(format!("--community={q}")));
            }

            let cancel = ctx.cancel.clone().unwrap_or_default();
            let cli_res = tokio::select! {
                () = cancel.cancelled() => return Err(ToolError::new("Tool execution cancelled")),
                res = run_ripwire_args(bin, &args) => res,
            };

            match cli_res {
                Ok(run) => {
                    stderr_note = trimmed_stderr(&run.stderr);
                    let trimmed = run.stdout.trim();
                    if trimmed.contains("<communit") {
                        let compacted = compact_ripwire_cli_output(trimmed);
                        let budgeted = truncate_budget(&compacted, max_tokens.saturating_mul(4));
                        let mut metadata = json!({
                            "operation": "communities",
                            "engine": "ripwire",
                            "transport": "cli",
                            "token_budget": max_tokens,
                        });
                        if let Some(err) = stderr_note {
                            metadata["ripwire_stderr"] = json!(err);
                        }
                        return Ok(ToolResult::text(budgeted).with_metadata(metadata));
                    }
                    failure =
                        Some("ripwire --communities returned non-community output".to_string());
                }
                Err(RipwireFailure::Timeout { timeout }) => {
                    return Err(timeout_error("communities", timeout));
                }
                Err(RipwireFailure::Spawn { message }) => {
                    failure = Some(message);
                }
                Err(RipwireFailure::Exit { code, stderr }) => {
                    stderr_note = trimmed_stderr(&stderr);
                    failure = Some(format!(
                        "ripwire exited with status {}",
                        code.map_or_else(|| "signal".to_string(), |c| c.to_string())
                    ));
                }
            }
        }

        let (mut out, modules) = native_communities(ctx, target_path, query, limit)?;
        let mut metadata = json!({
            "operation": "communities",
            "engine": "native",
            "modules": modules,
        });
        add_fallback_metadata(
            &mut out,
            &mut metadata,
            binary.is_none(),
            failure,
            stderr_note,
        );
        Ok(ToolResult::text(out).with_metadata(metadata))
    }

    pub(in crate::code_map) async fn execute_symbol(
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
            "symbol",
            "find_symbol",
            &json!({ "path": root, "symbol": query, "limit": limit }),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && let Some(formatted) =
                format_find_symbol_json(ctx, &root, rel_scope.as_deref(), query, &out.text)
        {
            let metadata = mcp_metadata("symbol", &out.stderr, &json!({ "query": query }));
            return Ok(finalize_precision(
                ctx,
                target_path,
                "symbol",
                query,
                precision,
                used_scip,
                formatted,
                metadata,
            )
            .await);
        }

        let (text, matches) = native_symbol(ctx, target_path, query, limit)?;
        let metadata = json!({
            "operation": "symbol",
            "engine": "native",
            "query": query,
            "matches": matches,
        });
        Ok(finalize_precision(
            ctx,
            target_path,
            "symbol",
            query,
            precision,
            false,
            text,
            metadata,
        )
        .await)
    }

    pub(in crate::code_map) async fn execute_expand(
        ctx: &ToolContext,
        target_path: &Path,
        query: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
        compress: bool,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let rel_scope = relative_scope(&root, target_path);

        let direct_handle = resolve_direct_handle(query, rel_scope.as_deref());
        let handle = if let Some(h) = direct_handle {
            Some(h)
        } else if let Some(McpAttempt::Ok(sym_out)) = run_mcp(
            ctx,
            &root,
            "expand",
            "find_symbol",
            &json!({ "path": root, "symbol": query, "limit": 1 }),
        )
        .await?
        {
            extract_handle_from_find_symbol(&sym_out.text, rel_scope.as_deref())
        } else {
            None
        };

        if let Some(h) = handle {
            let mut args = Map::new();
            args.insert("path".to_string(), json!(root));
            args.insert("handle".to_string(), json!(h));
            if let Some(s) = start_line {
                args.insert("start_line".to_string(), json!(s));
            }
            if let Some(e) = end_line {
                args.insert("end_line".to_string(), json!(e));
            }

            if let Some(McpAttempt::Ok(out)) =
                run_mcp(ctx, &root, "expand", "fetch_body", &Value::Object(args)).await?
                && let Some((formatted, file_rel)) =
                    format_fetch_body_json(ctx, &root, &out.text, compress)
            {
                return Ok(build_mcp_result(
                    "expand",
                    formatted,
                    &out.stderr,
                    &json!({ "query": query, "file": file_rel, "compress": compress }),
                ));
            }
        }

        let (text, file_rel) =
            native_expand(ctx, target_path, query, start_line, end_line, compress)?;
        Ok(ToolResult::text(text).with_metadata(json!({
            "operation": "expand",
            "engine": "native",
            "query": query,
            "file": file_rel,
            "compress": compress,
        })))
    }

    pub(in crate::code_map) async fn execute_impact(
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
            "impact",
            "impact",
            &json!({ "path": root, "symbol": query, "limit": limit }),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && !out.text.trim().is_empty()
        {
            let metadata = mcp_metadata("impact", &out.stderr, &json!({ "query": query }));
            return Ok(finalize_precision(
                ctx,
                target_path,
                "impact",
                query,
                precision,
                used_scip,
                compact_ripwire_cli_output(&out.text),
                metadata,
            )
            .await);
        }

        let (text, dependents) = native_impact(ctx, target_path, query, limit)?;
        let metadata = json!({
            "operation": "impact",
            "engine": "native",
            "query": query,
            "dependents": dependents,
        });
        Ok(finalize_precision(
            ctx,
            target_path,
            "impact",
            query,
            precision,
            false,
            text,
            metadata,
        )
        .await)
    }

    pub(in crate::code_map) async fn execute_connect(
        ctx: &ToolContext,
        target_path: &Path,
        query: &str,
        precision: PrecisionMode,
    ) -> Result<ToolResult, ToolError> {
        let (from, to) = parse_connect_endpoints(query).ok_or_else(|| {
            ToolError::new(
                "Operation 'connect' requires two symbol endpoints in `query`, e.g. `A -> B`",
            )
        })?;

        let root = tree_root_for(ctx, target_path);
        let (attempt, used_scip) = run_mcp_mode(
            ctx,
            &root,
            precision,
            "connect",
            "connect",
            &json!({ "path": root, "symbols": [from, to] }),
        )
        .await?;
        if let Some(McpAttempt::Ok(out)) = attempt
            && !out.text.trim().is_empty()
        {
            let metadata = mcp_metadata("connect", &out.stderr, &json!({ "from": from, "to": to }));
            return Ok(finalize_precision(
                ctx,
                target_path,
                "connect",
                &from,
                precision,
                used_scip,
                compact_ripwire_cli_output(&out.text),
                metadata,
            )
            .await);
        }

        let (text, hops) = native_connect(ctx, target_path, &from, &to)?;
        let metadata = json!({
            "operation": "connect",
            "engine": "native",
            "from": from,
            "to": to,
            "hops": hops,
        });
        Ok(finalize_precision(
            ctx,
            target_path,
            "connect",
            &from,
            precision,
            false,
            text,
            metadata,
        )
        .await)
    }

    pub(in crate::code_map) async fn execute_affected(
        ctx: &ToolContext,
        target_path: &Path,
        query: Option<&str>,
        limit: usize,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        let seed_csv = match query.map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => q.to_string(),
            None => detect_git_changed_csv(&root).await,
        };

        if seed_csv.is_empty() {
            return Ok(ToolResult::text(
                "No modified git files detected; pass comma-separated changed file paths or symbols in `query`.",
            )
            .with_metadata(json!({
                "operation": "affected",
                "engine": "native",
                "tests": 0,
            })));
        }

        if let Some(McpAttempt::Ok(out)) = run_mcp(
            ctx,
            &root,
            "affected",
            "affected",
            &json!({ "path": root, "files": seed_csv }),
        )
        .await?
            && !out.text.trim().is_empty()
        {
            return Ok(build_mcp_result(
                "affected",
                compact_ripwire_cli_output(&out.text),
                &out.stderr,
                &json!({ "seeds": seed_csv }),
            ));
        }

        let (text, tests) = native_affected(ctx, target_path, &seed_csv, limit)?;
        Ok(ToolResult::text(text).with_metadata(json!({
            "operation": "affected",
            "engine": "native",
            "seeds": seed_csv,
            "tests": tests,
        })))
    }

    pub(in crate::code_map) async fn execute_trace(
        ctx: &ToolContext,
        target_path: &Path,
        query: &str,
        max_tokens: usize,
    ) -> Result<ToolResult, ToolError> {
        let root = tree_root_for(ctx, target_path);
        if let Some(McpAttempt::Ok(out)) = run_mcp(
            ctx,
            &root,
            "trace",
            "from_trace",
            &json!({ "path": root, "trace": query, "budget_tokens": max_tokens }),
        )
        .await?
            && !out.text.trim().is_empty()
        {
            return Ok(build_mcp_result(
                "trace",
                compact_ripwire_cli_output(&out.text),
                &out.stderr,
                &json!({ "token_budget": max_tokens }),
            ));
        }

        let (text, resolved) = native_trace(ctx, target_path, query, max_tokens)?;
        Ok(ToolResult::text(text).with_metadata(json!({
            "operation": "trace",
            "engine": "native",
            "resolved": resolved,
        })))
    }
}
