//! Interactive permission prompts for terminal runs.

use std::io::Write;

use anyhow::bail;
use oc_types::permission::PermissionReply;
use serde_json::Value;
use tokio::io::{BufReader, Lines, Stdin};

use super::TerminalHost;

pub(super) enum PermissionPrompt<'a> {
    AutoApprove,
    Interactive(Option<&'a mut Lines<BufReader<Stdin>>>),
}

fn parse_permission_reply(input: &str) -> Option<PermissionReply> {
    match input.trim().to_ascii_lowercase().as_str() {
        "o" | "once" | "y" | "yes" | "1" => Some(PermissionReply::Once),
        "a" | "always" | "all" => Some(PermissionReply::Always),
        "" | "r" | "reject" | "n" | "no" | "d" | "deny" => Some(PermissionReply::Reject),
        _ => None,
    }
}

pub(super) async fn decide_permission_reply(
    host: &TerminalHost,
    session_id: &str,
    data: &Value,
    json_mode: bool,
    prompt_mode: &mut PermissionPrompt<'_>,
) -> anyhow::Result<PermissionReply> {
    let action = data.get("action").and_then(Value::as_str).unwrap_or("tool");
    match prompt_mode {
        PermissionPrompt::AutoApprove => {
            if !json_mode {
                eprintln!("\x1b[2m[permission] auto-approved {action}\x1b[0m");
            }
            Ok(PermissionReply::Once)
        }
        PermissionPrompt::Interactive(None) => {
            if !json_mode {
                eprintln!(
                    "\x1b[33m[permission] denied {action} (non-interactive stdin with --ask-permissions)\x1b[0m"
                );
            }
            Ok(PermissionReply::Reject)
        }
        PermissionPrompt::Interactive(Some(lines)) => {
            let resources: Vec<&str> = data
                .get("resources")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let message = data.get("message").and_then(Value::as_str);
            eprintln!();
            if resources.is_empty() {
                eprintln!("\x1b[33m[permission] {action}\x1b[0m");
            } else {
                eprintln!(
                    "\x1b[33m[permission] {action}: {}\x1b[0m",
                    resources.join(", ")
                );
            }
            if let Some(msg) = message
                && !msg.trim().is_empty()
            {
                eprintln!("  {msg}");
            }
            loop {
                eprint!("  Allow? [o]nce / [a]lways / [r]eject (default: reject): ");
                let _ = std::io::stderr().flush();
                let line = tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        let _ = host.runtime.interrupt(session_id).await;
                        eprintln!("\n\x1b[33m[interrupted]\x1b[0m");
                        bail!("execution interrupted by user");
                    }
                    next = lines.next_line() => match next {
                        Ok(Some(line)) => line,
                        Ok(None) | Err(_) => return Ok(PermissionReply::Reject),
                    }
                };
                if let Some(reply) = parse_permission_reply(&line) {
                    return Ok(reply);
                }
                eprintln!("  Please enter 'o' (once), 'a' (always), or 'r' (reject).");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_reply_parser_maps_choices_and_defaults_safely() {
        assert_eq!(parse_permission_reply("o"), Some(PermissionReply::Once));
        assert_eq!(parse_permission_reply("Once"), Some(PermissionReply::Once));
        assert_eq!(parse_permission_reply("  y "), Some(PermissionReply::Once));
        assert_eq!(parse_permission_reply("a"), Some(PermissionReply::Always));
        assert_eq!(
            parse_permission_reply("always"),
            Some(PermissionReply::Always)
        );
        assert_eq!(parse_permission_reply("r"), Some(PermissionReply::Reject));
        assert_eq!(
            parse_permission_reply("reject"),
            Some(PermissionReply::Reject)
        );
        assert_eq!(parse_permission_reply("n"), Some(PermissionReply::Reject));
        assert_eq!(parse_permission_reply(""), Some(PermissionReply::Reject));
        assert_eq!(parse_permission_reply("maybe"), None);
    }
}
