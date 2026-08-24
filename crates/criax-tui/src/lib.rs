//! The terminal UI layer: `Model`, `Msg`, `update`, `view`.
//!
//! # The one rule
//!
//! **Nothing in this crate performs I/O.** [`update`] is a pure, synchronous function
//! from `(&mut Model, Msg)` to a list of [`Effect`]s, and rendering only reads the model.
//! Network calls, database writes and file access happen in the effect runtime, which
//! lives in the binary crate and communicates over channels.
//!
//! This is enforced structurally: `criax-tui` does not depend on `reqwest`, `rusqlite` or
//! `tokio`, so it *cannot* await anything. That is deliberate. The project this replaces
//! awaited network calls while holding a lock on its application state, which froze the
//! terminal for the duration of every slow request. Do not add an I/O dependency here.
//!
//! # How a keystroke becomes a screen
//!
//! ```text
//! terminal event ─▶ Msg::Key ─▶ update ─▶ Effect::LoadTasks ─▶ (runtime, off-thread)
//!                                  │                                    │
//!                                  ▼                                    ▼
//!                              Model changed                     Msg::TasksLoaded
//!                                  │                                    │
//!                                  ▼                                    ▼
//!                               render ◀───────────────────────────── update
//! ```
//!
//! Two invariants hold that loop together. Every task query carries a
//! [`query::QueryId`], and an answer that is no longer current is dropped rather than
//! rendered — otherwise holding `j` down the sidebar paints whichever project's query
//! happened to finish last. And the list's selection is a [`criax_core::models::TaskId`],
//! never a row index, so a reload that reorders or drops rows cannot silently move the
//! cursor onto a different task.

pub mod effect;
pub mod geometry;
pub mod keymap;
pub mod modal;
pub mod model;
pub mod msg;
pub mod query;
pub mod rows;
pub mod sidebar;
pub mod theme;
pub mod update;
pub mod view;

pub use effect::Effect;
pub use keymap::{Action, Binding, Key, KEYMAP};
pub use modal::{Modal, ModalView};
pub use model::{Focus, Model, PaneState, Screen};
pub use msg::Msg;
pub use query::{Query, QueryId, Scope};
pub use update::update;
pub use view::view;
