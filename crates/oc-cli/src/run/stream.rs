//! Event streaming for a terminal turn: deltas, tool progress, settlement.

use std::collections::HashMap;
use std::io::Write;

use anyhow::bail;
use oc_types::EventEnvelope;
use serde_json::Value;
use tokio::sync::broadcast;

use super::TerminalHost;
use super::permission::{PermissionPrompt, decide_permission_reply};

#[allow(clippy::too_many_lines)]
pub(super) async fn stream_turn(
    host: &TerminalHost,
    session_id: &str,
    rx: &mut broadcast::Receiver<std::sync::Arc<EventEnvelope>>,
    json_mode: bool,
    prompt_mode: &mut PermissionPrompt<'_>,
) -> anyhow::Result<()> {
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut in_reasoning = false;
    let mut printed_text = false;

    loop {
        let envelope = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = host.runtime.interrupt(session_id).await;
                eprintln!("\n\x1b[33m[interrupted]\x1b[0m");
                bail!("execution interrupted by user");
            }
            recv = rx.recv() => match recv {
                Ok(envelope) => envelope,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        };

        if envelope.kind == "permission.asked" {
            let ask_session = envelope
                .data
                .get("sessionID")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let request_id = envelope
                .data
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !ask_session.is_empty() && !request_id.is_empty() {
                let reply = decide_permission_reply(
                    host,
                    session_id,
                    &envelope.data,
                    json_mode,
                    prompt_mode,
                )
                .await?;
                let _ = host.broker.reply(ask_session, request_id, reply, None);
            }
            continue;
        }

        let event_session = envelope
            .data
            .get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event_session != session_id {
            continue;
        }

        if json_mode {
            if let Ok(line) = serde_json::to_string(envelope.as_ref()) {
                println!("{line}");
            }
            match envelope.kind.as_str() {
                "session.execution.succeeded" => return Ok(()),
                "session.execution.failed" => {
                    let message = envelope
                        .data
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("execution failed");
                    bail!("{message}");
                }
                "session.execution.interrupted" => bail!("execution interrupted"),
                _ => continue,
            }
        }

        match envelope.kind.as_str() {
            "session.reasoning.delta" => {
                if let Some(delta) = envelope.data.get("delta").and_then(Value::as_str) {
                    if !in_reasoning {
                        eprint!("\x1b[2m");
                        in_reasoning = true;
                    }
                    eprint!("{delta}");
                    let _ = std::io::stderr().flush();
                }
            }
            "session.reasoning.ended" => {
                if in_reasoning {
                    eprintln!("\x1b[0m");
                    in_reasoning = false;
                }
            }
            "session.tool.input.started" => {
                if let (Some(id), Some(name)) = (
                    envelope.data.get("id").and_then(Value::as_str),
                    envelope.data.get("name").and_then(Value::as_str),
                ) {
                    tool_names.insert(id.to_owned(), name.to_owned());
                }
            }
            "session.tool.called" => {
                if in_reasoning {
                    eprintln!("\x1b[0m");
                    in_reasoning = false;
                }
                let id = envelope
                    .data
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = tool_names.get(id).map_or("tool", String::as_str);
                let summary = summarize_tool_input(envelope.data.get("input"));
                eprintln!("\x1b[36m▸ {name}\x1b[0m \x1b[2m{summary}\x1b[0m");
            }
            "session.tool.failed" => {
                let err = envelope
                    .data
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("tool failed");
                eprintln!("\x1b[31m✗ {err}\x1b[0m");
            }
            "session.text.delta" => {
                if in_reasoning {
                    eprintln!("\x1b[0m");
                    in_reasoning = false;
                }
                if let Some(delta) = envelope.data.get("delta").and_then(Value::as_str) {
                    print!("{delta}");
                    let _ = std::io::stdout().flush();
                    printed_text = true;
                }
            }
            "session.text.ended" => {
                if printed_text {
                    println!();
                    printed_text = false;
                }
            }
            "session.execution.succeeded" => {
                if in_reasoning {
                    eprintln!("\x1b[0m");
                }
                if printed_text {
                    println!();
                }
                return Ok(());
            }
            "session.execution.failed" => {
                if in_reasoning {
                    eprintln!("\x1b[0m");
                }
                let message = envelope
                    .data
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("execution failed");
                bail!("{message}");
            }
            "session.execution.interrupted" => {
                if in_reasoning {
                    eprintln!("\x1b[0m");
                }
                bail!("execution interrupted");
            }
            _ => {}
        }
    }
    Ok(())
}

fn summarize_tool_input(input: Option<&Value>) -> String {
    let Some(obj) = input.and_then(Value::as_object) else {
        return String::new();
    };
    for key in ["path", "command", "query", "url", "pattern", "id"] {
        if let Some(val) = obj.get(key).and_then(Value::as_str) {
            let single_line = val.lines().next().unwrap_or_default().trim();
            if single_line.chars().count() > 80 {
                let end = single_line
                    .char_indices()
                    .nth(80)
                    .map_or(single_line.len(), |(i, _)| i);
                return format!("{}…", &single_line[..end]);
            }
            return single_line.to_owned();
        }
    }
    String::new()
}
