//! Tool-call payloads.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::{MessageId, SessionId};
use crate::message::{Native, StructuredError, ToolContent};

use super::EventPayload;

/// `session.tool.input.started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolInputStarted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub id: String,
    pub name: String,
}

impl_event_payload!(
    SessionToolInputStarted,
    "session.tool.input.started",
    true,
    1
);

/// `session.tool.input.delta` (ephemeral).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolInputDelta {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub id: String,
    pub delta: String,
}

impl_event_payload!(SessionToolInputDelta, "session.tool.input.delta", false, 1);

/// `session.tool.input.ended`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolInputEnded {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub id: String,
    pub text: String,
}

impl_event_payload!(SessionToolInputEnded, "session.tool.input.ended", true, 1);

/// `session.tool.called`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolCalled {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub id: String,
    pub input: serde_json::Map<String, Value>,
    pub executed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl_event_payload!(SessionToolCalled, "session.tool.called", true, 1);

/// `session.tool.progress` (ephemeral).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolProgress {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub id: String,
    pub metadata: Native,
}

impl_event_payload!(SessionToolProgress, "session.tool.progress", false, 1);

/// `session.tool.success`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolSuccess {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub id: String,
    pub content: Vec<ToolContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Native>,
    pub executed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl_event_payload!(SessionToolSuccess, "session.tool.success", true, 2);

/// `session.tool.failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolFailed {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub id: String,
    pub error: StructuredError,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolContent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Native>,
    pub executed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl_event_payload!(SessionToolFailed, "session.tool.failed", true, 2);
