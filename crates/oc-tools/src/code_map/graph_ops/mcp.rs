//! `ripwire --mcp` transport: attempt classification, precision finalization,
//! metadata/result builders and scope matching shared by every graph operation.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::super::precision::{PrecisionMode, append_lsp_precision, scip_for_mode};
use super::super::ripwire::{
    RIPWIRE_INSTALL_CMD, configured_ripwire_timeout, find_ripwire_binary, trimmed_stderr,
};
use super::super::ripwire_mcp::{McpCallError, McpToolOutput, call_mcp_tool_with_scip};

use crate::{ToolContext, ToolError, ToolResult};

/// Outcome of attempting an operation against the persistent `ripwire --mcp` pool.
pub(in crate::code_map) enum McpAttempt {
    Ok(McpToolOutput),
    CliOnly { stderr: Vec<u8> },
    Fallback { error: String, stderr: Vec<u8> },
}

pub(in crate::code_map) fn timeout_error(operation: &str, timeout: Duration) -> ToolError {
    ToolError::new(format!(
        "ripwire did not finish within {} ms; raise OPENCODE_RIPWIRE_TIMEOUT_MS \
         or pass `query` to narrow the scan",
        timeout.as_millis()
    ))
    .with_metadata(json!({
        "operation": operation,
        "engine": "ripwire",
        "timeout_ms": timeout.as_millis(),
    }))
}

pub(super) async fn run_mcp(
    ctx: &ToolContext,
    root: &Path,
    operation: &str,
    tool_name: &str,
    arguments: &Value,
) -> Result<Option<McpAttempt>, ToolError> {
    run_mcp_with_scip(ctx, root, None, operation, tool_name, arguments).await
}

pub(super) async fn run_mcp_with_scip(
    ctx: &ToolContext,
    root: &Path,
    scip_path: Option<&Path>,
    operation: &str,
    tool_name: &str,
    arguments: &Value,
) -> Result<Option<McpAttempt>, ToolError> {
    if ctx
        .cancel
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(ToolError::new("Tool execution cancelled"));
    }
    let Some(binary) = find_ripwire_binary() else {
        return Ok(None);
    };
    let cancel = ctx.cancel.clone().unwrap_or_default();
    let timeout = configured_ripwire_timeout();

    match call_mcp_tool_with_scip(
        &binary, root, scip_path, &cancel, timeout, tool_name, arguments,
    )
    .await
    {
        Ok(out) => Ok(Some(McpAttempt::Ok(out))),
        Err(McpCallError::Timeout { timeout }) => Err(timeout_error(operation, timeout)),
        Err(McpCallError::Cancelled) => Err(ToolError::new("Tool execution cancelled")),
        Err(McpCallError::CliOnly { stderr }) => Ok(Some(McpAttempt::CliOnly { stderr })),
        Err(McpCallError::Tool { message, stderr } | McpCallError::Io { message, stderr }) => {
            Ok(Some(McpAttempt::Fallback {
                error: message,
                stderr,
            }))
        }
    }
}

pub(in crate::code_map) async fn run_mcp_mode(
    ctx: &ToolContext,
    root: &Path,
    precision: PrecisionMode,
    operation: &str,
    tool_name: &str,
    arguments: &Value,
) -> Result<(Option<McpAttempt>, bool), ToolError> {
    let scip = scip_for_mode(root, precision);
    let used_scip = scip.is_some();
    let attempt =
        run_mcp_with_scip(ctx, root, scip.as_deref(), operation, tool_name, arguments).await?;
    Ok((attempt, used_scip))
}

pub(crate) fn parse_mcp_json_text(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(trimmed).ok().or_else(|| {
        let end = trimmed.rfind('}')?;
        serde_json::from_str::<Value>(&trimmed[..=end]).ok()
    })
}

pub(super) fn record_scip_meta(metadata: &mut Value, precision: PrecisionMode, used_scip: bool) {
    if precision != PrecisionMode::Fast {
        metadata["precision"] = json!(precision.as_str());
    }
    if used_scip {
        metadata["scip_overlay"] = json!(true);
    }
}

pub(in crate::code_map) fn add_fallback_metadata(
    out: &mut String,
    metadata: &mut Value,
    binary_missing: bool,
    failure: Option<String>,
    stderr: Option<String>,
) {
    if binary_missing {
        let _ = writeln!(
            out,
            "\n[Tip: For deep call graphs and Personalized PageRank, install ripwire: `{RIPWIRE_INSTALL_CMD}`]"
        );
        metadata["ripwire_install_command"] = json!(RIPWIRE_INSTALL_CMD);
    } else if let Some(reason) = failure {
        metadata["ripwire_error"] = json!(reason);
    }
    if let Some(err) = stderr {
        metadata["ripwire_stderr"] = json!(err);
    }
}

pub(super) fn matches_scope(rel_file: &str, rel_scope: Option<&str>) -> bool {
    let Some(prefix) = rel_scope else {
        return true;
    };
    let norm = rel_file.replace('\\', "/");
    norm == prefix || norm.starts_with(&format!("{prefix}/"))
}

pub(super) fn mcp_metadata(operation: &str, stderr: &[u8], extra: &Value) -> Value {
    let mut metadata = json!({
        "operation": operation,
        "engine": "ripwire",
        "transport": "mcp",
    });
    if let (Some(dst), Some(src)) = (metadata.as_object_mut(), extra.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    if let Some(s) = trimmed_stderr(stderr) {
        metadata["ripwire_stderr"] = json!(s);
    }
    metadata
}

pub(super) fn build_mcp_result(
    operation: &str,
    text: String,
    stderr: &[u8],
    extra: &Value,
) -> ToolResult {
    ToolResult::text(text).with_metadata(mcp_metadata(operation, stderr, extra))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn finalize_precision(
    ctx: &ToolContext,
    target_path: &Path,
    op: &str,
    query: &str,
    precision: PrecisionMode,
    used_scip: bool,
    mut out: String,
    mut metadata: Value,
) -> ToolResult {
    record_scip_meta(&mut metadata, precision, used_scip);
    if !query.is_empty() {
        append_lsp_precision(
            ctx,
            target_path,
            op,
            query,
            precision,
            &mut out,
            &mut metadata,
        )
        .await;
    }
    ToolResult::text(out).with_metadata(metadata)
}
