//! Step, status, idle and usage payloads.

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId};
use crate::message::{FinishReason, Native, StructuredError};
use crate::session::{ModelRef, SessionStatus, Tokens};

use super::EventPayload;

/// `session.step.started`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStepStarted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub agent: String,
    pub model: ModelRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    pub started: i64,
}

impl_event_payload!(SessionStepStarted, "session.step.started", true, 1);

/// `session.step.streamed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStepStreamed {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
}

impl_event_payload!(SessionStepStreamed, "session.step.streamed", true, 1);

/// `session.step.ended`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStepEnded {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub finish: FinishReason,
    #[serde(rename = "rawFinish", skip_serializing_if = "Option::is_none")]
    pub raw_finish: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
    pub cost: f64,
    pub tokens: Tokens,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

impl_event_payload!(SessionStepEnded, "session.step.ended", true, 1);

/// `session.step.failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStepFailed {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub error: StructuredError,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish: Option<FinishReason>,
    #[serde(rename = "rawFinish", skip_serializing_if = "Option::is_none")]
    pub raw_finish: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

impl_event_payload!(SessionStepFailed, "session.step.failed", true, 1);

/// `session.status` (ephemeral).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusUpdated {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub status: SessionStatus,
}

impl_event_payload!(SessionStatusUpdated, "session.status", false, 1);

/// `session.idle` (ephemeral).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdle {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
}

impl_event_payload!(SessionIdle, "session.idle", false, 1);

/// `session.usage.updated` (ephemeral).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageUpdated {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub cost: f64,
    pub tokens: Tokens,
}

impl_event_payload!(SessionUsageUpdated, "session.usage.updated", false, 1);
