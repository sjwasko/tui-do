//! Wire types for the Vikunja API.
//!
//! These are hand-written rather than generated from `spec/vikunja.json`, because the
//! spec under-describes the wire format in ways a generator cannot recover from: every
//! timestamp is a bare `{"type": "string"}` with no format, and Vikunja encodes "unset"
//! as Go's zero time rather than `null` (see [`datetime`]). Generated types would be
//! `String` fields plus a hand-written conversion layer — the same work, one indirection
//! further from the truth.
//!
//! Drift is caught instead by `tests/conformance.rs`, which checks these structs'
//! fields against the spec's definitions and fails when the two disagree. That covers
//! what codegen would have, and additionally catches fields we have *stopped* modelling.

pub mod datetime;

mod ids;
mod label;
mod project;
mod task;
mod user;

pub use ids::{LabelId, ProjectId, TaskId, UserId, ViewId};
pub use label::Label;
pub use project::{Project, ProjectView, ViewKind};
pub use task::{Bucket, RelationKind, RepeatMode, Task, TaskAttachment, TaskComment, TaskReminder};
pub use user::User;
