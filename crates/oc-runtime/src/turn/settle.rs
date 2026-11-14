use oc_store::NewMessage;
use oc_types::payload::{
    SessionExecutionFailed, SessionExecutionInterrupted, SessionExecutionStepLimit,
    SessionExecutionSucceeded, SessionIdle, SessionStatusUpdated,
};
use oc_types::{InterruptReason, MessageId, SessionStatus};
use serde_json::{Map, Value, json};

use crate::runtime::{Runtime, RuntimeError, TerminalState};

use super::types::ExecutionOutcome;

/// Lowercase wire value of an [`InterruptReason`] (`user`, `shutdown`,
/// `superseded`, `inactivity`), used for the durable `session_v2.idle_reason`.
fn interrupt_reason_str(reason: InterruptReason) -> &'static str {
    match reason {
        InterruptReason::User => "user",
        InterruptReason::Shutdown => "shutdown",
        InterruptReason::Superseded => "superseded",
        InterruptReason::Inactivity => "inactivity",
    }
}

impl Runtime {
    pub(super) async fn settle(
        &self,
        session: &str,
        directory: &str,
        outcome: ExecutionOutcome,
        terminal: &TerminalState,
    ) -> Result<(), RuntimeError> {
        match &outcome {
            ExecutionOutcome::Failed(_) => self.note_observer_execution(session, true).await,
            ExecutionOutcome::Interrupted(_) => self.note_observer_execution(session, false).await,
            ExecutionOutcome::Succeeded | ExecutionOutcome::StepLimited { .. } => {}
        }
        let now = oc_types::now_millis();
        // The *stored* outcome is the honest internal literal `step_limit`
        // (a completed turn whose step budget ended it), while the *wire*
        // terminal stays `session.execution.succeeded` and the idle message
        // keeps the literal `succeeded`: `SessionOutcome` has exactly three
        // literals, so a fourth one on a wire event/message would break the
        // generated-types contract. `oc_types::SessionInfo` reads an unknown
        // stored outcome as absent instead of failing, which is what lets the
        // next execution of this session parse at all.
        let (stored, name, error, reason) = match &outcome {
            ExecutionOutcome::Succeeded => ("succeeded", "succeeded", None, None),
            ExecutionOutcome::StepLimited { .. } => ("step_limit", "succeeded", None, None),
            ExecutionOutcome::Failed(error) => ("failed", "failed", Some(error.clone()), None),
            ExecutionOutcome::Interrupted(reason) => (
                "interrupted",
                "interrupted",
                None,
                Some(interrupt_reason_str(*reason)),
            ),
        };
        // A step-limited answer keeps the step-limit grammar as its reason;
        // the store persists it next to the `step_limit` outcome.
        let reason = match &outcome {
            ExecutionOutcome::StepLimited { steps } => Some(oc_types::step_limit_reason(*steps)),
            _ => reason.map(str::to_owned),
        };
        // Hoisted out of the variant before `publish_outcome_terminal` consumes
        // the outcome. The qualifier is emitted only with the first terminal.
        let step_limit = match &outcome {
            ExecutionOutcome::StepLimited { steps } => Some(*steps),
            _ => None,
        };
        self.clear_background_request(session);
        // The idle row and its event are committed/published before the outcome
        // event, so a crash between them leaves a session without an
        // `execution.*` terminal rather than a terminal followed by a busy row.
        self.publish_idle_terminal(
            session,
            directory,
            now,
            stored,
            name,
            error.as_ref(),
            reason.as_deref(),
            terminal,
        )
        .await?;
        let first_outcome = !terminal.outcome_published();
        self.publish_outcome_terminal(session, directory, outcome, terminal)
            .await?;
        // The qualifier is published strictly after the terminal event is
        // durable, so its sequence number orders it after `succeeded`. Guarded
        // by the same idempotency flag as the terminal itself.
        if first_outcome && let Some(steps) = step_limit {
            self.emit(
                session,
                directory,
                SessionExecutionStepLimit {
                    session_id: session.to_owned(),
                    steps,
                },
            )
            .await?;
            // A step-limited turn is not a failure, so the arms above skip it
            // and a timer tick would never evaluate a now-idle root: by this
            // point the idle row says `step limit reached after N steps` and
            // nothing else is busy, pending or overdue. Wake the observer
            // through the same host-flag path the failure/interruption arms
            // use, after the idle commit above, so the next pass renders that
            // reason. `HostFlag` has no step-limit variant on purpose (it is a
            // `Copy` enum with an exhaustive match); the two waking flags only
            // set `pending_wake` and `last_activity_ms`
            // (`oc-observer/src/engine/run.rs:75-78`), so no observer state
            // records this as an interruption. A granted turn never gets here:
            // a grant continues the loop instead of returning `StepLimited`.
            self.note_observer_execution(session, false).await;
        }
        Ok(())
    }

    /// Writes the idle row, then publishes `session.idle` and
    /// `session.status{idle}`.
    ///
    /// `stored` is the internal `session_v2.idle_outcome` literal (it may be
    /// `step_limit`, which the wire does not know); `name` is the wire literal
    /// for the persisted idle message. Both are written in the same step, so the
    /// durable row and the transcript can never disagree about *that* a turn
    /// settled — only about the internal qualifier.
    ///
    /// Idempotent: the terminal guard records that the pair was published so a
    /// later recovery pass (the actor failure path) does not duplicate the
    /// ephemeral events.
    // Settle helper: the flat parameter list mirrors the persisted idle row and
    // its event fields one-for-one; grouping it into a struct would only add
    // ceremony at the two call sites.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn publish_idle_terminal(
        &self,
        session: &str,
        directory: &str,
        now: i64,
        stored: &str,
        name: &str,
        error: Option<&oc_types::StructuredError>,
        reason: Option<&str>,
        terminal: &TerminalState,
    ) -> Result<(), RuntimeError> {
        if terminal.idle_published() {
            return Ok(());
        }
        self.inner
            .config
            .store
            .mark_session_idle_with_reason(session, now, stored, reason)
            .await?;
        let mut idle = Map::new();
        idle.insert("time".to_owned(), json!({ "created": now }));
        idle.insert("outcome".to_owned(), Value::String(name.to_owned()));
        if let Some(error) = error {
            idle.insert(
                "error".to_owned(),
                serde_json::to_value(error).expect("structured error is serializable"),
            );
        }
        let idle_id = MessageId::new();
        self.inner
            .config
            .store
            .append_message(NewMessage {
                id: idle_id.as_str().to_owned(),
                session_id: session.to_owned(),
                kind: "idle".to_owned(),
                data: Value::Object(idle),
                time_created: now,
            })
            .await?;
        self.emit(
            session,
            directory,
            SessionIdle {
                session_id: Runtime::session_id(session),
            },
        )
        .await?;
        self.emit(
            session,
            directory,
            SessionStatusUpdated {
                session_id: Runtime::session_id(session),
                status: SessionStatus::Idle,
            },
        )
        .await?;
        terminal.mark_idle();
        Ok(())
    }

    /// Publishes the `session.execution.*` outcome exactly once per turn.
    pub(super) async fn publish_outcome_terminal(
        &self,
        session: &str,
        directory: &str,
        outcome: ExecutionOutcome,
        terminal: &TerminalState,
    ) -> Result<(), RuntimeError> {
        if terminal.outcome_published() {
            return Ok(());
        }
        match outcome {
            // A step-limited turn keeps the same wire terminal as a voluntary
            // stop; the qualifier is `session.execution.step_limit`, emitted by
            // `settle` after this event is durable.
            ExecutionOutcome::Succeeded | ExecutionOutcome::StepLimited { .. } => {
                self.emit(
                    session,
                    directory,
                    SessionExecutionSucceeded {
                        session_id: Runtime::session_id(session),
                    },
                )
                .await?;
            }
            ExecutionOutcome::Failed(error) => {
                self.emit(
                    session,
                    directory,
                    SessionExecutionFailed {
                        session_id: Runtime::session_id(session),
                        error,
                    },
                )
                .await?;
            }
            ExecutionOutcome::Interrupted(reason) => {
                self.emit(
                    session,
                    directory,
                    SessionExecutionInterrupted {
                        session_id: Runtime::session_id(session),
                        reason,
                    },
                )
                .await?;
            }
        }
        terminal.mark_outcome();
        Ok(())
    }

    /// Closes every assistant/tool/compaction row the crashed turn left open,
    /// reusing the boot-recovery repair so the two paths cannot drift.
    pub(super) async fn repair_open_turn_rows(&self, session: &str) -> Result<(), RuntimeError> {
        for message in self.list_all_messages(session).await? {
            let repaired = match message.get("type").and_then(Value::as_str) {
                Some("assistant") => self.repair_assistant(&message).await?,
                Some("compaction") => crate::recover::repair_compaction(&message),
                _ => None,
            };
            let Some(body) = repaired else {
                continue;
            };
            let id = message
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.inner
                .config
                .store
                .update_message(session, id, body)
                .await?;
        }
        Ok(())
    }
}
