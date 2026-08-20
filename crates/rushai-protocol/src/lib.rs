//! Wire types shared by the rushai engine and its frontends.
//!
//! Frontends submit [`Op`]s; the engine emits [`Event`]s. This seam is the
//! only coupling between the two, so headless and TUI modes share one core.

mod events;
mod ids;
mod ops;
mod parts;
mod permission;
mod usage;

pub use events::Event;
pub use ids::{CallId, MessageId, RequestId, SessionId};
pub use ops::Op;
pub use parts::{FinishReason, Part, PartDelta, Role, UserPart};
pub use permission::{Decision, PermissionRequest};
pub use usage::TokenUsage;
