use std::sync::Arc;

use oc_types::message::StructuredError;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::expectation::{self, Expectation};
use crate::runtime::{CancelToken, Runtime, RuntimeError};

use super::assistant::AssistantState;
use super::types::{PendingToolCall, ResolvedTurn, ToolOutcome};

mod outcome;
mod run;
mod spill;

use outcome::{settle_flight_strip, settle_open};
use run::run_tool;

#[cfg(test)]
use oc_types::message::ToolContent;
#[cfg(test)]
use outcome::tool_content;

#[derive(Clone)]
struct ToolProgressSender {
    index: usize,
    call_id: String,
    tx: mpsc::UnboundedSender<(usize, String, Map<String, Value>)>,
}

tokio::task_local! {
    static TOOL_PROGRESS: ToolProgressSender;
}

/// Emit intermediate metadata for the currently executing tool call in this task.
pub(crate) fn report_tool_progress(metadata: Map<String, Value>) {
    let _ = TOOL_PROGRESS.try_with(|sender| {
        let _ = sender
            .tx
            .send((sender.index, sender.call_id.clone(), metadata));
    });
}

enum ToolStepWait {
    Cancelled,
    Progress((usize, String, Map<String, Value>)),
    Joined(Option<Result<(usize, ToolOutcome), tokio::task::JoinError>>),
}

const MAX_TOOL_PARALLELISM: usize = 8;
/// Maximum characters kept inline inside a single tool text block (~12k tokens).
/// Larger outputs are spilled to `.opencode/tool-output/<call-id>.txt` and
/// replaced inline with head + spillover path notice + tail.
const MAX_INLINE_TOOL_OUTPUT_CHARS: usize = 48_000;
const SPILL_HEAD_CHARS: usize = 20_000;
const SPILL_TAIL_CHARS: usize = 10_000;

#[derive(Clone)]
struct StripStartContext {
    root_id: String,
    depth: u32,
    open: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Runtime {
    pub(super) async fn execute_tools(
        &self,
        session: &str,
        turn: &ResolvedTurn,
        state: &mut AssistantState,
        calls: &[PendingToolCall],
        cancel: &Arc<CancelToken>,
    ) -> Result<(), RuntimeError> {
        let observer = self.observer();
        let (root_id, depth) = if observer.is_some() {
            match self.inner.config.store.session_node(session).await {
                Ok(Some(node)) => (node.root_id, node.depth),
                Ok(None) => (session.to_owned(), 0),
                Err(error) => return Err(error.into()),
            }
        } else {
            (String::new(), 0)
        };
        let open: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut set = tokio::task::JoinSet::new();
        let (progress_tx, mut progress_rx) =
            mpsc::unbounded_channel::<(usize, String, Map<String, Value>)>();
        let mut next = 0_usize;
        while next < calls.len() || !set.is_empty() {
            while set.len() < MAX_TOOL_PARALLELISM && next < calls.len() {
                let call = calls[next].clone();
                let index = call.index;
                let runtime = self.clone();
                let turn = turn.clone();
                let message_id = state.id.clone();
                let token = Arc::clone(cancel);
                let strip_ctx = StripStartContext {
                    root_id: root_id.clone(),
                    depth,
                    open: Arc::clone(&open),
                };
                let sender = ToolProgressSender {
                    index,
                    call_id: call.id.clone(),
                    tx: progress_tx.clone(),
                };
                set.spawn(TOOL_PROGRESS.scope(sender, async move {
                    (
                        index,
                        run_tool(runtime, turn, message_id, call, token, strip_ctx).await,
                    )
                }));
                next += 1;
            }
            let wait = tokio::select! {
                biased;
                () = cancel.cancelled() => ToolStepWait::Cancelled,
                Some(progress) = progress_rx.recv() => ToolStepWait::Progress(progress),
                joined = set.join_next() => ToolStepWait::Joined(joined),
            };
            let joined = match wait {
                ToolStepWait::Cancelled => {
                    self.abort_cancelled_tools(session, turn, state, &open, &mut set)
                        .await;
                    return Err(RuntimeError::Cancelled);
                }
                ToolStepWait::Progress((index, call_id, metadata)) => {
                    self.apply_tool_progress(session, turn, state, index, call_id, metadata)
                        .await?;
                    continue;
                }
                ToolStepWait::Joined(joined) => joined,
            };
            while let Ok((p_index, p_call_id, p_meta)) = progress_rx.try_recv() {
                self.apply_tool_progress(session, turn, state, p_index, p_call_id, p_meta)
                    .await?;
            }
            match joined {
                Some(Ok((_index, ToolOutcome::Cancelled))) => {
                    self.abort_cancelled_tools(session, turn, state, &open, &mut set)
                        .await;
                    return Err(RuntimeError::Cancelled);
                }
                Some(Ok((index, outcome))) => {
                    settle_flight_strip(
                        observer.as_deref(),
                        &root_id,
                        session,
                        &turn.agent_id,
                        depth,
                        calls,
                        &open,
                        index,
                        &outcome,
                    );
                    self.apply_tool_outcome(session, turn, state, outcome)
                        .await?;
                }
                Some(Err(error)) => {
                    settle_open(observer.as_deref(), &open, "task failed");
                    self.clear_background_request(session);
                    return Err(RuntimeError::Task(error.to_string()));
                }
                None => break,
            }
        }
        self.clear_background_request(session);
        Ok(())
    }
}
pub(super) fn strip_expectation(
    tool: &str,
    input: Value,
) -> (
    Map<String, Value>,
    Option<Expectation>,
    Option<StructuredError>,
) {
    let mut value = match input {
        Value::String(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(parsed @ Value::Object(_)) => parsed,
            Ok(_) => {
                return (
                    Map::new(),
                    Some(Expectation::derived(tool)),
                    Some(StructuredError::new(
                        "InvalidToolArgumentsError",
                        format!(
                            "Tool arguments for {tool:?} must be a JSON object; re-invoke {tool:?} with valid JSON arguments."
                        ),
                    )),
                );
            }
            Err(error) => {
                return (
                    Map::new(),
                    Some(Expectation::derived(tool)),
                    Some(StructuredError::new(
                        "InvalidToolArgumentsError",
                        format!(
                            "Tool arguments for {tool:?} are not valid JSON ({error}); re-invoke {tool:?} with valid JSON arguments."
                        ),
                    )),
                );
            }
        },
        other => other,
    };
    match expectation::extract(&mut value, tool) {
        Ok(expectation) => (
            value.as_object().cloned().unwrap_or_default(),
            Some(expectation),
            None,
        ),
        Err(error) => {
            tracing::warn!(tool = %tool, error = %error, "invalid expectation; falling back to derived");
            (
                value.as_object().cloned().unwrap_or_default(),
                Some(Expectation::derived(tool)),
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_tool_output_stays_inline_without_spilling() {
        let result = oc_tools::ToolResult::text("hello world");
        let (parts, metadata) = tool_content(&result, ".", "call_small");
        assert_eq!(
            parts,
            vec![ToolContent::Text {
                text: "hello world".to_owned()
            }]
        );
        assert!(metadata.is_none());
    }

    #[test]
    fn oversized_tool_output_spills_to_disk_and_truncates_inline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir_str = dir.path().to_string_lossy().into_owned();
        let big = format!(
            "HEAD_MARKER_{}_{}_TAIL_MARKER",
            "a".repeat(30_000),
            "b".repeat(30_000)
        );
        let result = oc_tools::ToolResult::text(big.clone());
        let (parts, metadata) = tool_content(&result, &dir_str, "call_oversized_1");
        assert_eq!(parts.len(), 1);
        let ToolContent::Text { text } = &parts[0] else {
            panic!("expected text part");
        };
        assert!(text.starts_with("HEAD_MARKER_"));
        assert!(text.ends_with("_TAIL_MARKER"));
        assert!(text.contains("Full untruncated output saved to"));
        assert!(text.chars().count() < MAX_INLINE_TOOL_OUTPUT_CHARS);

        let meta = metadata.expect("metadata");
        assert_eq!(meta.get("truncated").and_then(Value::as_bool), Some(true));
        let spill_path = meta
            .get("spillPath")
            .and_then(Value::as_str)
            .expect("spillPath");
        let saved = std::fs::read_to_string(spill_path).expect("read spilled file");
        assert_eq!(saved, big);
    }
}
