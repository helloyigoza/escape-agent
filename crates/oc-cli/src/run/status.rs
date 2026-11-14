//! Chat help text and `/status` rendering.

use serde_json::Value;

use super::TerminalHost;

pub(super) fn print_chat_help() {
    eprintln!("Available commands:");
    eprintln!("  /new                  Start a fresh session");
    eprintln!("  /compact              Compact session transcript into a structured summary");
    eprintln!(
        "  /model <provider/id>  Switch the session model (e.g. anthropic/claude-sonnet-4-5)"
    );
    eprintln!("  /agent <id>           Switch the session agent (e.g. build, plan)");
    eprintln!("  /status               Show current session, agent, model, and message count");
    eprintln!(
        "  /ag [cmd]             Manage Antigravity pool (/ag status|quota|use|enable|disable|models)"
    );
    eprintln!("  /help                 Show this help message");
    eprintln!("  /exit                 Exit interactive chat");
}

pub(super) async fn print_session_status(
    host: &TerminalHost,
    session_id: &str,
) -> anyhow::Result<()> {
    let session = host
        .services
        .store
        .get_session(session_id)
        .await?
        .unwrap_or(Value::Null);
    let agent = session
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_else(|| host.services.config.default_agent());
    let model = session
        .get("model")
        .and_then(|m| {
            let p = m.get("providerID")?.as_str()?;
            let id = m.get("id")?.as_str()?;
            Some(format!("{p}/{id}"))
        })
        .or_else(|| host.services.config.model())
        .unwrap_or_else(|| "(catalog default)".to_owned());
    let page = host
        .services
        .store
        .list_messages(oc_store::MessageQuery {
            session_id: session_id.to_owned(),
            limit: 500,
            order: oc_store::Order::Asc,
            cursor: None,
            kind: None,
        })
        .await?;
    eprintln!("Session:  {session_id}");
    eprintln!("Agent:    {agent}");
    eprintln!("Model:    {model}");
    eprintln!("Messages: {}", page.items.len());
    Ok(())
}
