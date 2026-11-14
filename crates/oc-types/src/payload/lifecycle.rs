//! Session and execution lifecycle payloads.

use serde::{Deserialize, Serialize};

use crate::id::{ProjectId, SessionId};
use crate::message::{Native, StructuredError};
use crate::permission::PermissionRule;
use crate::session::{ModelRef, SessionLocation};

use super::EventPayload;

/// `session.created`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreated {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "projectID")]
    pub project_id: ProjectId,
    pub location: SessionLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    #[serde(rename = "parentID", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<SessionId>,
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Native>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<PermissionRule>>,
    pub version: String,
}

impl_event_payload!(SessionCreated, "session.created", true, 1);

/// `session.renamed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenamed {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub title: String,
}

impl_event_payload!(SessionRenamed, "session.renamed", true, 1);

/// `session.deleted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
}

impl_event_payload!(SessionDeleted, "session.deleted", true, 2);

/// `session.execution.started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExecutionStarted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
}

impl_event_payload!(
    SessionExecutionStarted,
    "session.execution.started",
    true,
    1
);

/// `session.execution.succeeded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExecutionSucceeded {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
}

impl_event_payload!(
    SessionExecutionSucceeded,
    "session.execution.succeeded",
    true,
    1
);

/// `session.execution.step_limit` — published after the terminal event when a
/// turn stopped because the step budget was exhausted. The turn still publishes
/// `session.execution.succeeded`; this event is the honest qualifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExecutionStepLimit {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub steps: u64,
}

impl_event_payload!(
    SessionExecutionStepLimit,
    "session.execution.step_limit",
    true,
    1
);

/// `session.execution.failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExecutionFailed {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub error: StructuredError,
}

impl_event_payload!(SessionExecutionFailed, "session.execution.failed", true, 1);

/// `Session.Execution.InterruptReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterruptReason {
    User,
    Shutdown,
    Superseded,
    Inactivity,
}

/// `session.execution.interrupted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExecutionInterrupted {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub reason: InterruptReason,
}

impl_event_payload!(
    SessionExecutionInterrupted,
    "session.execution.interrupted",
    true,
    1
);
/// `session.archived`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionArchived {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
}

impl_event_payload!(SessionArchived, "session.archived", true, 1);

/// `session.unarchived`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUnarchived {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
}

impl_event_payload!(SessionUnarchived, "session.unarchived", true, 1);

/// `session.moved`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMoved {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub location: SessionLocation,
    #[serde(rename = "projectID")]
    pub project_id: ProjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
}

impl_event_payload!(SessionMoved, "session.moved", true, 1);

/// `session.viewed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionViewed {
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub idle: i64,
}

impl_event_payload!(SessionViewed, "session.viewed", true, 1);
