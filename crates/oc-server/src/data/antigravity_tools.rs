//! Runtime `ToolProvider` exposing Antigravity's grounded `google_search` tool.

use std::sync::Arc;

use oc_antigravity::{AntigravityService, GoogleSearchTool};
use oc_tools::Tool;
use oc_types::SessionInfo;

/// Runtime tool provider backed by [`AntigravityService`].
#[derive(Debug, Clone)]
pub struct AntigravityToolProvider {
    service: Arc<AntigravityService>,
}

impl AntigravityToolProvider {
    /// Wrap an initialized [`AntigravityService`].
    #[must_use]
    pub const fn new(service: Arc<AntigravityService>) -> Self {
        Self { service }
    }
}

impl oc_runtime::ToolProvider for AntigravityToolProvider {
    fn tools(&self, _session: &SessionInfo, _agent: &str) -> Vec<Arc<dyn Tool>> {
        if self.service.has_usable_accounts() {
            vec![Arc::new(GoogleSearchTool::new(
                self.service.as_ref().clone(),
            ))]
        } else {
            Vec::new()
        }
    }
}
