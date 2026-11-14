//! `ConfigService` inherent `impl`, split by cluster.
//!
//! The type and its fields stay in the crate root (the facade), so child
//! modules extend it with inherent `impl` blocks and every `pub use` path is
//! unchanged. The per-directory cache bound is shared by [skills] and
//! [commands], so it lives here and children import it through `super`.

mod agents;
mod commands;
mod ctor;
mod misc;
mod skills;

/// Upper bound on cached per-directory instruction sets. Reaching it clears the
/// map instead of evicting one entry; a re-walk is cheap and correctness does
/// not depend on the cache.
const MAX_INSTRUCTION_CACHE: usize = 128;
