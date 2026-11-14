//! Scalar accessors: MCP servers, shell, model, default agent, diagnostics and
//! the normalized documents.

use serde_json::Value;

use crate::ConfigService;
use crate::diagnostic::Diagnostic;

impl ConfigService {
    /// Merged `mcp.servers` object (empty when unconfigured).
    #[must_use]
    pub fn mcp_servers(&self) -> Value {
        self.mcp_servers.clone()
    }

    /// Configured shell, or the platform default.
    #[must_use]
    pub fn shell(&self) -> String {
        self.shell.clone()
    }

    /// Default model reference as `provider/model[#variant]`.
    #[must_use]
    pub fn model(&self) -> Option<String> {
        self.model.clone()
    }

    /// Selected default primary agent.
    #[must_use]
    pub fn default_agent(&self) -> &str {
        &self.default_agent
    }

    /// Warnings collected while loading: dropped legacy keys, malformed
    /// documents, missing `{file:...}` references.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Normalized documents in load order.
    #[must_use]
    pub fn documents(&self) -> &[Value] {
        &self.documents
    }
}
