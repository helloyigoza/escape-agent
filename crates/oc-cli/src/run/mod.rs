//! `opencode run` and `opencode chat`: terminal execution and interactive REPL.
//!
//! This facade owns the two entry points and the shared [`TerminalHost`]. Host
//! bootstrapping, session plumbing and model validation live in [`host`]; the
//! turn event loop in [`stream`]; interactive permission prompts in
//! [`permission`]; chat help and `/status` rendering in [`status`].

use std::io::{IsTerminal, Write};

use oc_server::AppState;
use oc_store::SessionPatch;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::cli::{ChatOptions, RunOptions};

mod host;
mod permission;
mod status;
mod stream;

use self::host::{
    boot_terminal_host, enqueue_compaction, enqueue_user_prompt, ensure_session,
    parse_and_validate_model, resolve_directory, resolve_prompt_text, validate_agent,
};
use self::permission::PermissionPrompt;
use self::status::{print_chat_help, print_session_status};
use self::stream::stream_turn;

/// Execute a single prompt in the terminal and exit when the turn settles.
pub(crate) async fn run_once(options: RunOptions) -> anyhow::Result<()> {
    let RunOptions {
        prompt,
        model,
        agent,
        directory,
        session,
        db,
        json: json_mode,
        ask_permissions,
    } = options;

    let text = resolve_prompt_text(&prompt)?;
    let directory = resolve_directory(directory)?;
    let host = boot_terminal_host(&directory, db).await?;

    let model_ref = match model.as_deref() {
        Some(raw) => Some(parse_and_validate_model(&host, raw)?),
        None => None,
    };
    if let Some(agent_id) = agent.as_deref() {
        validate_agent(&host.services.config, agent_id)?;
    }

    let session_id = ensure_session(
        &host,
        &directory,
        session.as_deref(),
        agent.as_deref(),
        model_ref.as_ref(),
    )
    .await?;

    let mut rx = host.state.bus().subscribe();
    enqueue_user_prompt(&host.state, &session_id, &text).await?;

    let mut stdin_lines = (ask_permissions && !json_mode && std::io::stdin().is_terminal())
        .then(|| BufReader::new(tokio::io::stdin()).lines());
    let mut prompt_mode = if ask_permissions {
        PermissionPrompt::Interactive(stdin_lines.as_mut())
    } else {
        PermissionPrompt::AutoApprove
    };
    let outcome = stream_turn(&host, &session_id, &mut rx, json_mode, &mut prompt_mode).await;
    host.shutdown().await;
    outcome
}

/// Start an interactive multi-turn terminal chat session.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_chat(options: ChatOptions) -> anyhow::Result<()> {
    let ChatOptions {
        model,
        agent,
        directory,
        session,
        db,
        ask_permissions,
    } = options;

    let directory = resolve_directory(directory)?;
    let host = boot_terminal_host(&directory, db).await?;

    let model_ref = match model.as_deref() {
        Some(raw) => Some(parse_and_validate_model(&host, raw)?),
        None => None,
    };
    if let Some(agent_id) = agent.as_deref() {
        validate_agent(&host.services.config, agent_id)?;
    }

    let mut session_id = ensure_session(
        &host,
        &directory,
        session.as_deref(),
        agent.as_deref(),
        model_ref.as_ref(),
    )
    .await?;

    eprintln!(
        "\x1b[1;36mopencode\x1b[0m v{} • session \x1b[2m{}\x1b[0m • dir \x1b[2m{}\x1b[0m",
        env!("CARGO_PKG_VERSION"),
        session_id,
        directory.display()
    );
    eprintln!(
        "\x1b[2mCommands: /new, /compact, /model <provider/id>, /agent <id>, /status, /help, /exit\x1b[0m\n"
    );

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    loop {
        eprint!("\x1b[1;32mopencode>\x1b[0m ");
        let _ = std::io::stderr().flush();

        let line = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!();
                break;
            }
            next = reader.next_line() => match next {
                Ok(Some(line)) => line,
                Ok(None) => {
                    eprintln!();
                    break;
                }
                Err(error) => {
                    eprintln!("\x1b[31mread error: {error}\x1b[0m");
                    break;
                }
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "/exit" | "/quit" | ":q") {
            break;
        }
        if trimmed == "/help" {
            print_chat_help();
            continue;
        }
        if trimmed == "/new" {
            session_id = ensure_session(
                &host,
                &directory,
                None,
                agent.as_deref(),
                model_ref.as_ref(),
            )
            .await?;
            eprintln!("\x1b[2m[new session: {session_id}]\x1b[0m");
            continue;
        }
        if trimmed == "/status" {
            print_session_status(&host, &session_id).await?;
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("/antigravity")
            .or_else(|| trimmed.strip_prefix("/ag"))
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            if let Err(error) = crate::antigravity_cmd::handle_chat_command(
                host.services.antigravity.as_deref(),
                rest.trim(),
            )
            .await
            {
                eprintln!("\x1b[31m{error}\x1b[0m");
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/model") {
            let arg = rest.trim();
            if arg.is_empty() {
                print_session_status(&host, &session_id).await?;
                continue;
            }
            match parse_and_validate_model(&host, arg) {
                Ok(parsed) => {
                    host.services
                        .store
                        .patch_session(
                            &session_id,
                            SessionPatch {
                                model: Some(json!(&parsed)),
                                ..SessionPatch::default()
                            },
                        )
                        .await?;
                    eprintln!(
                        "\x1b[2m[model set to {}/{}]\x1b[0m",
                        parsed.provider_id, parsed.id
                    );
                }
                Err(error) => eprintln!("\x1b[31m{error}\x1b[0m"),
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/agent") {
            let arg = rest.trim();
            if arg.is_empty() {
                print_session_status(&host, &session_id).await?;
                continue;
            }
            match validate_agent(&host.services.config, arg) {
                Ok(()) => {
                    host.services
                        .store
                        .patch_session(
                            &session_id,
                            SessionPatch {
                                agent: Some(arg.to_owned()),
                                ..SessionPatch::default()
                            },
                        )
                        .await?;
                    eprintln!("\x1b[2m[agent set to {arg}]\x1b[0m");
                }
                Err(error) => eprintln!("\x1b[31m{error}\x1b[0m"),
            }
            continue;
        }
        if matches!(trimmed, "/compact" | "/compress") {
            let mut rx = host.state.bus().subscribe();
            enqueue_compaction(&host.state, &session_id).await?;
            let interactive_tty = ask_permissions && std::io::stdin().is_terminal();
            let mut prompt_mode = if ask_permissions {
                PermissionPrompt::Interactive(interactive_tty.then_some(&mut reader))
            } else {
                PermissionPrompt::AutoApprove
            };
            if let Err(error) =
                stream_turn(&host, &session_id, &mut rx, false, &mut prompt_mode).await
            {
                eprintln!("\x1b[31m{error}\x1b[0m");
            } else {
                eprintln!("\x1b[2m[session compacted]\x1b[0m");
            }
            continue;
        }

        let mut rx = host.state.bus().subscribe();
        enqueue_user_prompt(&host.state, &session_id, trimmed).await?;
        let interactive_tty = ask_permissions && std::io::stdin().is_terminal();
        let mut prompt_mode = if ask_permissions {
            PermissionPrompt::Interactive(interactive_tty.then_some(&mut reader))
        } else {
            PermissionPrompt::AutoApprove
        };
        if let Err(error) = stream_turn(&host, &session_id, &mut rx, false, &mut prompt_mode).await
        {
            eprintln!("\x1b[31m{error}\x1b[0m");
        }
        println!();
    }

    host.shutdown().await;
    Ok(())
}

struct TerminalHost {
    services: crate::bootstrap::Services,
    extensions: crate::bootstrap::Extensions,
    state: AppState,
    runtime: std::sync::Arc<oc_runtime::Runtime>,
    broker: std::sync::Arc<oc_server::PermissionBroker>,
}

impl TerminalHost {
    async fn shutdown(self) {
        self.extensions.lsp.shutdown_all().await;
        self.extensions.lanes.shutdown().await;
        self.runtime.shutdown().await;
        self.broker.reject_all("The CLI session is shutting down.");
    }
}
