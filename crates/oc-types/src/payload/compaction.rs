//! Compaction and inbox payloads.

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId};
use crate::inbox::{Delivery, InboxItem};
use crate::message::{Native, StructuredError};
use crate::session::{ModelRef, Tokens};

use super::EventPayload;

/// `session.compaction.started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactionStarted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub reason: String,
    #[serde(rename = "inputID", skip_serializing_if = "Option::is_none")]
    pub input_id: Option<MessageId>,
}

impl_event_payload!(
    SessionCompactionStarted,
    "session.compaction.started",
    true,
    1
);

/// `session.compaction.ended`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactionEnded {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
}

impl_event_payload!(SessionCompactionEnded, "session.compaction.ended", true, 1);

/// `session.compaction.failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactionFailed {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub reason: String,
    pub error: StructuredError,
    #[serde(rename = "inputID", skip_serializing_if = "Option::is_none")]
    pub input_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
}

impl_event_payload!(
    SessionCompactionFailed,
    "session.compaction.failed",
    true,
    1
);

/// `session.inbox.enqueued`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInboxEnqueued {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "inboxID")]
    pub inbox_id: MessageId,
    pub item: InboxItem,
}

impl_event_payload!(SessionInboxEnqueued, "session.inbox.enqueued", true, 1);

/// `session.inbox.delivered`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInboxDelivered {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "inboxID")]
    pub inbox_id: MessageId,
}

impl_event_payload!(SessionInboxDelivered, "session.inbox.delivered", true, 1);

/// `session.inbox.cancelled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInboxCancelled {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "inboxID")]
    pub inbox_id: MessageId,
}

impl_event_payload!(SessionInboxCancelled, "session.inbox.cancelled", true, 1);

/// `session.inbox.delivery.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInboxDeliveryChanged {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "inboxID")]
    pub inbox_id: MessageId,
    pub delivery: Delivery,
}

impl_event_payload!(
    SessionInboxDeliveryChanged,
    "session.inbox.delivery.changed",
    true,
    1
);
