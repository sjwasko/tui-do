//! The terminal UI layer: `Model`, `Msg`, `update`, `view`.
//!
//! # The one rule
//!
//! **Nothing in this crate performs I/O.** `update` is a pure, synchronous function from
//! `(&mut Model, Msg)` to a list of [`Effect`]s, and `view` only reads the model. Network
//! calls, database writes and file access happen in the effect runtime, which lives in the
//! binary crate and communicates over channels.
//!
//! This is enforced structurally: `criax-tui` does not depend on `reqwest`, `rusqlite` or
//! `tokio`, so it *cannot* await anything. That is deliberate. The project this replaces
//! awaited network calls while holding a lock on its application state, which froze the
//! terminal for the duration of every slow request. Do not add an I/O dependency here.

/// A side effect requested by `update`, to be executed by the effect runtime.
///
/// Populated in Phase 1; the enum exists now so the boundary is visible from commit one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Leave the application.
    Quit,
}
