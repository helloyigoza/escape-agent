//! Text and reasoning part payloads.

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId};
use crate::message::Native;

use super::EventPayload;

/// `session.text.started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTextStarted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub ordinal: u32,
}

impl_event_payload!(SessionTextStarted, "session.text.started", true, 1);

/// `session.text.delta` (ephemeral).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTextDelta {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub ordinal: u32,
    pub delta: String,
}

impl_event_payload!(SessionTextDelta, "session.text.delta", false, 1);

/// `session.text.ended`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTextEnded {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub ordinal: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl_event_payload!(SessionTextEnded, "session.text.ended", true, 1);

/// `session.reasoning.started`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReasoningStarted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub ordinal: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl_event_payload!(
    SessionReasoningStarted,
    "session.reasoning.started",
    true,
    1
);

/// `session.reasoning.delta` (ephemeral).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReasoningDelta {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub ordinal: u32,
    pub delta: String,
}

impl_event_payload!(SessionReasoningDelta, "session.reasoning.delta", false, 1);

/// `session.reasoning.ended`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReasoningEnded {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "assistantMessageID")]
    pub assistant_message_id: MessageId,
    pub ordinal: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl_event_payload!(SessionReasoningEnded, "session.reasoning.ended", true, 1);
