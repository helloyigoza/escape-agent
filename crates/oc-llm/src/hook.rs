//! Optional transport hook for delegating provider requests (such as OAuth
//! gateways) while reusing [`LlmClient`](crate::LlmClient) and
//! [`SharedLlmClient`](crate::SharedLlmClient).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::catalog::{Catalog, ModelInfo, ResolvedModel};
use crate::client::SharedLlmClient;
use crate::error::LlmError;
use crate::event::{LlmRequest, LlmStream};

/// Boxed future returned by [`LlmTransportHook::stream`].
pub type HookStreamFuture = Pin<Box<dyn Future<Output = Result<LlmStream, LlmError>> + Send>>;

/// Intercepts matching [`LlmRequest`] instances before built-in HTTP dispatch.
pub trait LlmTransportHook: Send + Sync + fmt::Debug {
    /// Returns `true` when this hook should execute `request` instead of the
    /// built-in protocol sender.
    fn handles(&self, request: &LlmRequest) -> bool;

    /// Executes `request` and returns a stream of [`LlmEvent`](crate::LlmEvent).
    fn stream(&self, request: LlmRequest) -> HookStreamFuture;

    /// Resolve a `provider/id[#variant]` reference dynamically when models or
    /// variants were discovered after the initial static [`Catalog`] snapshot.
    fn resolve_model(&self, _catalog: &Catalog, _reference: &str) -> Option<ResolvedModel> {
        None
    }

    /// Return dynamically discovered or updated [`ModelInfo`] rows from the hook.
    fn dynamic_models(&self) -> Vec<ModelInfo> {
        Vec::new()
    }
}

impl SharedLlmClient {
    /// Attaches a transport hook to the underlying [`LlmClient`](crate::LlmClient).
    pub fn set_hook(&self, hook: Option<Arc<dyn LlmTransportHook>>) {
        self.0.set_hook(hook);
    }

    /// Returns the currently attached transport hook, if any.
    #[must_use]
    pub fn hook(&self) -> Option<Arc<dyn LlmTransportHook>> {
        self.0.hook()
    }

    /// Resolve `reference` against `catalog`, falling back to any attached hook's
    /// dynamically discovered model catalog when missing from the startup snapshot.
    #[allow(
        clippy::result_large_err,
        reason = "mirrors Catalog::resolve's typed LlmError return"
    )]
    pub fn resolve_model(
        &self,
        catalog: &Catalog,
        reference: &str,
    ) -> Result<ResolvedModel, LlmError> {
        match catalog.resolve(reference) {
            Ok(resolved) => Ok(resolved),
            Err(err) => {
                if let Some(hook) = self.hook()
                    && let Some(resolved) = hook.resolve_model(catalog, reference)
                {
                    return Ok(resolved);
                }
                Err(err)
            }
        }
    }

    /// Return dynamically discovered models supplied by an attached hook.
    #[must_use]
    pub fn dynamic_models(&self) -> Vec<ModelInfo> {
        self.hook()
            .map_or_else(Vec::new, |hook| hook.dynamic_models())
    }
}
