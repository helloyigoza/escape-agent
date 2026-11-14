//! Model resolution and per-family request transformations for Antigravity Cloud Code Assist.

pub mod claude;
pub mod cross_model;
pub mod gemini;
pub mod model_resolver;
pub mod request;

pub use model_resolver::{ResolvedTargetModel, ThinkingMode, resolve_antigravity_model};
pub use request::{TransformedRequest, build_cloud_code_request};
